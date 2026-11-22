//! `harw admin init|passwd|totp|status|serve`: Admin-Konto und der
//! Enrollment-Webdienst für Geräte (`appadm`-Seite).
//!
//! # Beschreibung
//! Das Konto liegt unter `<home>/admin/` ([`account`]), der Webdienst
//! ([`web`]) bindet ausschließlich Loopback und wird in der Regel über einen
//! Tunnel veröffentlicht. Die öffentliche Adresse steht **nie im Code**:
//! [`public_base`] liest `[admin] public_url` der globalen `config.toml`;
//! fehlt sie, gibt harw nur die lokale URL und einen Hinweis aus.
//!
//! # Fehler
//! Deutsche `String`-Fehler ohne Geheimnisse.

mod account;
mod audit;
mod enroll;
mod totp;
mod web;

use std::io::{BufRead as _, Write as _};
use std::net::SocketAddr;
use std::path::Path;

use harw_config::{AdminSection, ResolvedConfig};
use secrecy::SecretString;
use zeroize::Zeroizing;

use crate::cli::{AdminAction, GlobalArgs};
use account::Account;

/// Hinweis, wenn `public_url` fehlt.
const NO_PUBLIC_URL_HINT: &str = "Hinweis: [admin] public_url fehlt in der globalen config.toml. \
Ohne sie gibt harw nur die lokale Adresse aus; Handy und Deep-Links brauchen die öffentliche \
Basis-URL (z. B. public_url = \"https://admin.example.org\").";

/// Führt `harw admin …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext.
pub(crate) fn run(g: &GlobalArgs, action: AdminAction) -> Result<(), String> {
    let home = crate::home::resolve_home(g.home.clone())?;
    let config = load_config(&home)?;
    match action {
        AdminAction::Init => init(&home, &config),
        AdminAction::Passwd => passwd(&home),
        AdminAction::Totp { disable } => totp_cmd(&home, &config, disable),
        AdminAction::Status => status(&home, &config),
        AdminAction::Serve => web::serve(&home, &config),
    }
}

fn load_config(home: &Path) -> Result<ResolvedConfig, String> {
    let layers = harw_home::config_layers(home).map_err(|error| error.to_string())?;
    harw_config::discover_config(&layers).map_err(|error| error.to_string())
}

/// Validiert `[admin] public_url` und liefert die normalisierte Basis ohne
/// abschließenden Schrägstrich. `Ok(None)`: nicht gesetzt.
///
/// # Errors
/// Kein `https://` (nur Loopback darf `http://`), Zugangsdaten, Query oder
/// Fragment in der URL.
pub(crate) fn public_base(admin: &AdminSection) -> Result<Option<String>, String> {
    let Some(raw) = admin.public_url.as_deref() else {
        return Ok(None);
    };
    let value = raw.trim().trim_end_matches('/');
    let bad = || "[admin] public_url muss https://host[:port] sein (http nur für Loopback)".to_owned();
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|b| b.is_ascii_graphic())
        || value.contains(['@', '?', '#', '\\'])
    {
        return Err(bad());
    }
    let host_part = if let Some(rest) = value.strip_prefix("https://") {
        rest
    } else if let Some(rest) = value
        .strip_prefix("http://127.0.0.1")
        .or_else(|| value.strip_prefix("http://localhost"))
    {
        if !(rest.is_empty() || rest.starts_with(':') || rest.starts_with('/')) {
            return Err(bad());
        }
        return Ok(Some(value.to_owned()));
    } else {
        return Err(bad());
    };
    if host_part.is_empty() || host_part.starts_with(['/', ':']) {
        return Err(bad());
    }
    Ok(Some(value.to_owned()))
}

/// Der Host (mit Port, wenn angegeben) einer Basis-URL, für die Host-Prüfung.
pub(crate) fn host_of(base: &str) -> String {
    base.split_once("://")
        .map_or(base, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Gibt die Adresszeilen aus (öffentlich aus der Config, sonst lokal + Hinweis).
fn address_lines(admin: &AdminSection) -> Vec<String> {
    let mut out = Vec::new();
    match public_base(admin) {
        Ok(Some(base)) => out.push(format!("Öffentliche Adresse: {base}/")),
        Ok(None) => out.push(NO_PUBLIC_URL_HINT.to_owned()),
        Err(error) => out.push(format!("Fehler: {error}")),
    }
    out.push(format!("Lokale Adresse:      http://{}/", admin.listen));
    out
}

fn prompt_line(label: &str) -> Result<String, String> {
    print!("{label}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim().to_owned())
}

fn prompt_password(label: &str) -> Result<Zeroizing<String>, String> {
    rpassword::prompt_password(label)
        .map(Zeroizing::new)
        .map_err(|error| format!("Passwort nicht lesbar: {error}"))
}

/// Fragt zweimal nach einem neuen Passwort und erzwingt die Regeln.
fn new_password() -> Result<Zeroizing<String>, String> {
    let first = prompt_password("Passwort: ")?;
    account::check_password_policy(&first)?;
    let second = prompt_password("Passwort wiederholen: ")?;
    if *first != *second {
        return Err("Die Passwörter stimmen nicht überein.".to_owned());
    }
    Ok(first)
}

fn init(home: &Path, config: &ResolvedConfig) -> Result<(), String> {
    if Account::load(home)?.is_some() {
        return Err(
            "Es gibt bereits ein Admin-Konto. `harw admin passwd` ändert das Passwort, `harw admin totp` das TOTP."
                .to_owned(),
        );
    }
    let username = prompt_line("Benutzername: ")?;
    account::check_username(&username)?;
    let email = prompt_line("E-Mail: ")?;
    account::check_email(&email)?;
    let password = new_password()?;
    let hash = account::hash_password(&password)?;
    drop(password);
    let mut account = Account::new(username, email, hash);
    account.save(home)?;
    audit::record(home, "admin_init", "cli", serde_json::json!({ "user": account.username }));
    println!("Admin-Konto '{}' angelegt ({}).", account.username, Account::path(home).display());
    let answer = prompt_line("TOTP (Zwei-Faktor) jetzt einrichten? [J/n]: ")?;
    if !answer.eq_ignore_ascii_case("n") {
        setup_totp(home, config, &mut account)?;
    }
    println!();
    for line in address_lines(&config.harness.admin) {
        println!("{line}");
    }
    println!("Start des Webdienstes: harw admin serve");
    Ok(())
}

fn passwd(home: &Path) -> Result<(), String> {
    let mut account = Account::load(home)?
        .ok_or_else(|| "Kein Admin-Konto vorhanden: zuerst `harw admin init`.".to_owned())?;
    let password = new_password()?;
    account.password_hash = account::hash_password(&password)?;
    drop(password);
    account.clear_failures();
    account.save(home)?;
    audit::record(home, "admin_passwd", "cli", serde_json::json!({ "user": account.username }));
    println!("Passwort geändert; Fehlversuche und Sperre zurückgesetzt.");
    println!("Hinweis: laufende Web-Sitzungen enden spätestens mit ihrer Lebensdauer; `harw admin serve` neu starten beendet sie sofort.");
    Ok(())
}

fn totp_cmd(home: &Path, config: &ResolvedConfig, disable: bool) -> Result<(), String> {
    let mut account = Account::load(home)?
        .ok_or_else(|| "Kein Admin-Konto vorhanden: zuerst `harw admin init`.".to_owned())?;
    if disable {
        if let Some(reference) = account.totp_secret_ref.take() {
            crate::secret_store::discard_sealed(home, config, &reference);
        }
        account.totp_last_step = 0;
        account.save(home)?;
        audit::record(home, "admin_totp_disabled", "cli", serde_json::json!({}));
        println!("TOTP abgeschaltet.");
        return Ok(());
    }
    setup_totp(home, config, &mut account)
}

/// Erzeugt ein TOTP-Geheimnis, zeigt URI und QR, bestätigt mit einem Code und
/// versiegelt das Geheimnis im Secret-Store.
fn setup_totp(home: &Path, config: &ResolvedConfig, account: &mut Account) -> Result<(), String> {
    let secret = Zeroizing::new(totp::generate_secret()?);
    let uri = Zeroizing::new(totp::otpauth_uri("harw", &account.username, &secret));
    println!("Authenticator-App (z. B. Aegis, 2FAS, Google Authenticator) hinzufügen:");
    if let Some(qr) = totp::qr_text(&uri) {
        println!("{qr}");
    }
    println!("otpauth-URI: {}", uri.as_str());
    println!("Manuell: Schlüssel {} (SHA1, 6 Stellen, 30 s)", totp::base32(&secret));
    let code = prompt_line("Aktuellen 6-stelligen Code zur Bestätigung eingeben: ")?;
    let step = totp::verify(&secret, &code, account::now_unix(), 0)
        .ok_or_else(|| "Code falsch; TOTP wurde nicht aktiviert.".to_owned())?;
    let sealed = SecretString::from(totp::base32(&secret));
    let reference = crate::secret_store::store_secret(
        home,
        config,
        "admin-totp",
        "admin-web-totp",
        &sealed,
    )?
    .as_ref_string();
    if let Some(old) = account.totp_secret_ref.replace(reference) {
        crate::secret_store::discard_sealed(home, config, &old);
    }
    account.totp_last_step = step;
    account.save(home)?;
    audit::record(home, "admin_totp_enabled", "cli", serde_json::json!({}));
    println!("TOTP aktiviert (Geheimnis versiegelt im Secret-Store).");
    Ok(())
}

fn mode_text(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        return match std::fs::metadata(path) {
            Ok(meta) => format!("Modus {:04o}", meta.permissions().mode() & 0o777),
            Err(_) => "nicht vorhanden".to_owned(),
        };
    }
    #[allow(unreachable_code)]
    if path.exists() { "vorhanden".to_owned() } else { "nicht vorhanden".to_owned() }
}

fn status(home: &Path, config: &ResolvedConfig) -> Result<(), String> {
    let admin = &config.harness.admin;
    match Account::load(home)? {
        None => println!("Admin-Konto: nicht angelegt (`harw admin init`)."),
        Some(account) => {
            let now = account::now_unix();
            println!("Admin-Konto: {} <{}>", account.username, account.email);
            println!("  TOTP:           {}", if account.totp_secret_ref.is_some() { "aktiv" } else { "aus" });
            println!("  Fehlversuche:   {}", account.failed_count);
            if account.is_locked(now) {
                println!("  Gesperrt noch:  {} s", account.locked_until - now);
            }
            println!("  Datei:          {} ({})", Account::path(home).display(), mode_text(&Account::path(home)));
        }
    }
    println!("Audit-Log:       {} ({})", audit::path(home).display(), mode_text(&audit::path(home)));
    println!("Webdienst:       listen = {}", admin.listen);
    match admin.listen.parse::<SocketAddr>() {
        Ok(addr) if addr.ip().is_loopback() => {}
        _ => println!("  Warnung: [admin] listen ist keine Loopback-Adresse; `harw admin serve` startet so nicht."),
    }
    for line in address_lines(admin) {
        println!("{line}");
    }
    println!(
        "Gateway-Neustart nach Freigabe: {}",
        if admin.restart_gateway {
            format!("ja (systemctl --user restart {})", admin.gateway_unit)
        } else {
            "nein (manuell nötig)".to_owned()
        }
    );
    Ok(())
}
