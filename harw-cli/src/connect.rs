//! Interaktiver, fail-closed Telegram-Setup- und Pairing-Flow.
//!
//! Der Bot-Token wird nie persistiert: die Channel-Konfiguration enthält nur
//! `env:HARW_TELEGRAM_BOT_TOKEN`. Pairing akzeptiert nur eine frische,
//! einmalige lokale Pairing-Referenz und bindet sie an die Telegram-ID der
//! Nachricht, die den Code tatsächlich gesendet hat.

use std::fs;
use std::path::{Path, PathBuf};

use harw_channel::PairingStore;
use harw_config::{ChannelFileToml, SecretRef, TelegramChannelToml};
use harw_types::{ChannelId, PeerId, TenantId};
use jiff::Timestamp;
use serde::Deserialize;

const TOKEN_ENV: &str = "HARW_TELEGRAM_BOT_TOKEN";
const CHANNEL_ID: &str = "telegram:default";
const TENANT: &str = "default";
const PAIR_PREFIX: &str = "/pair ";

pub fn run(home: &Path, channel: &str, pair_code: Option<&str>) -> Result<(), String> {
    if channel != "telegram" {
        return Err(format!(
            "unbekannter Channel {channel:?}; derzeit wird nur 'telegram' unterstützt"
        ));
    }
    crate::home::ensure_home(home).map_err(|error| error.to_string())?;
    if let Some(code) = pair_code {
        pair(home, code)
    } else {
        setup(home)
    }
}

fn setup(home: &Path) -> Result<(), String> {
    let token = token_from_environment()?;
    let bot = telegram_get_me(&token)?;
    let path = channel_path(home)?;
    if path.exists() {
        return Err(format!(
            "Telegram ist bereits konfiguriert ({}). Zum Pairing sende dem Bot '/pair <Code>' und führe 'harw connect --channel telegram --pair <Code>' aus.",
            path.display()
        ));
    }

    let channel = TelegramChannelToml {
        id: CHANNEL_ID.to_owned(),
        tenant_binding: TENANT.to_owned(),
        bot_token_ref: format!("env:{TOKEN_ENV}")
            .parse()
            .map_err(|e: harw_config::ConfigError| e.to_string())?,
        transport: "long_poll".to_owned(),
        // Bis zum Pairing ist der Gateway bewusst nicht startbar.
        enabled: false,
        transport_webhook: None,
        groups: Default::default(),
        topics: Default::default(),
        security: Default::default(),
        rate_limit: Default::default(),
        attachments: Default::default(),
        commands: Default::default(),
    };
    let mut document = ChannelFileToml::default();
    document.channel.telegram.push(channel);
    write_channel(&path, document)?;

    let code = issue_code(home)?;
    println!(
        "Telegram-Bot @{} wurde verifiziert.",
        bot.username.as_deref().unwrap_or("unbekannt")
    );
    println!("Telegram ist vorbereitet, aber noch deaktiviert.");
    println!("1. Schreibe dem Bot genau: {PAIR_PREFIX}{code}");
    println!("2. Führe danach aus: harw connect --channel telegram --pair {code}");
    println!(
        "Der Code ist einmalig und läuft nach kurzer Zeit ab. Der Bot-Token bleibt ausschließlich in ${TOKEN_ENV}."
    );
    Ok(())
}

fn pair(home: &Path, code: &str) -> Result<(), String> {
    let path = channel_path(home)?;
    let mut document = load_channel(&path)?;
    let binding = document.channel.telegram.iter_mut().find(|entry| entry.id == CHANNEL_ID)
        .ok_or_else(|| "keine Telegram-Standardbindung gefunden; zuerst 'harw connect --channel telegram' ausführen".to_owned())?;
    let token = resolve_token_ref(&binding.bot_token_ref)?;
    let peer = find_pair_message(&token, code)?;

    let store = PairingStore::new(&profile_dir(home)?.join("channel-state").join("pairing"));
    let channel: ChannelId = CHANNEL_ID
        .parse()
        .map_err(|_| "interne ungültige Channel-ID".to_owned())?;
    let peer_id: PeerId = peer
        .to_string()
        .parse()
        .map_err(|_| "ungültige Telegram-Peer-ID".to_owned())?;
    store
        .redeem_once(&channel, code, &peer_id, Timestamp::now())
        .map_err(|error| format!("Pairing wurde nicht übernommen: {error}"))?;

    if !binding.security.pinned_identities.contains(&peer) {
        binding.security.pinned_identities.push(peer);
    }
    binding.enabled = true;
    write_channel(&path, document)?;
    println!("Telegram-ID {peer} wurde gepairt und angepinnt. Starte jetzt: harw gateway");
    Ok(())
}

fn issue_code(home: &Path) -> Result<String, String> {
    let mut entropy = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut entropy))
        .map_err(|_| {
            "sicherer Zufall für den Pairing-Code konnte nicht gelesen werden".to_owned()
        })?;
    let store = PairingStore::new(&profile_dir(home)?.join("channel-state").join("pairing"));
    let channel: ChannelId = CHANNEL_ID
        .parse()
        .map_err(|_| "interne ungültige Channel-ID".to_owned())?;
    let tenant: TenantId = TENANT
        .parse()
        .map_err(|_| "interne ungültige Tenant-ID".to_owned())?;
    store
        .issue_code(&channel, &tenant, &entropy, Timestamp::now())
        .map_err(|error| format!("Pairing-Code konnte nicht angelegt werden: {error}"))
}

fn profile_dir(home: &Path) -> Result<PathBuf, String> {
    harw_home::profile_dir(home, &harw_home::active_profile_name(home))
        .map_err(|error| error.to_string())
}
fn channel_path(home: &Path) -> Result<PathBuf, String> {
    Ok(profile_dir(home)?.join("channels").join("telegram.toml"))
}
fn load_channel(path: &Path) -> Result<ChannelFileToml, String> {
    let source = fs::read_to_string(path).map_err(|_| {
        format!(
            "Channel-Datei {} konnte nicht gelesen werden",
            path.display()
        )
    })?;
    toml::from_str(&source).map_err(|error| format!("Channel-Datei ist ungültig: {error}"))
}
fn write_channel(path: &Path, document: ChannelFileToml) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Channel-Pfad hat kein Elternverzeichnis".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let rendered = toml::to_string_pretty(&document).map_err(|error| error.to_string())?;
    fs::write(path, rendered)
        .map_err(|error| format!("Channel-Konfiguration konnte nicht geschrieben werden: {error}"))
}
fn token_from_environment() -> Result<String, String> {
    std::env::var(TOKEN_ENV).ok().filter(|value| !value.trim().is_empty()).ok_or_else(|| format!("{TOKEN_ENV} ist nicht gesetzt. Setze den BotFather-Token vor dem Setup als Umgebungsvariable; Tokens werden nicht in Konfigurationen gespeichert."))
}
fn resolve_token_ref(reference: &SecretRef) -> Result<String, String> {
    match reference {
        SecretRef::Env(name) => std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("Telegram-Token aus env:{name} ist nicht verfügbar")),
        _ => Err("Telegram-Setup unterstützt ausschließlich env:-Token-Referenzen".to_owned()),
    }
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: T,
}
#[derive(Deserialize)]
struct Bot {
    username: Option<String>,
}
#[derive(Deserialize)]
struct Update {
    message: Option<Message>,
}
#[derive(Deserialize)]
struct Message {
    text: Option<String>,
    chat: Chat,
    from: Option<User>,
}
#[derive(Deserialize)]
struct Chat {
    id: i64,
}
#[derive(Deserialize)]
struct User {
    id: i64,
}

fn telegram_get_me(token: &str) -> Result<Bot, String> {
    telegram_request(token, "getMe", None)
}
fn find_pair_message(token: &str, code: &str) -> Result<i64, String> {
    let updates: Vec<Update> = telegram_request(
        token,
        "getUpdates",
        Some("timeout=0&allowed_updates=%5B%22message%22%5D"),
    )?;
    let expected = format!("{PAIR_PREFIX}{code}");
    updates.into_iter().rev().find_map(|update| {
        let message = update.message?;
        (message.text.as_deref() == Some(expected.as_str()) && message.from.as_ref().is_some_and(|user| user.id == message.chat.id)).then_some(message.chat.id)
    }).ok_or_else(|| format!("keine private Telegram-Nachricht mit {expected:?} gefunden. Sende sie dem Bot und wiederhole den Befehl; der Gateway darf während des Pairings nicht pollen."))
}
fn telegram_request<T: for<'de> Deserialize<'de>>(
    token: &str,
    method: &str,
    query: Option<&str>,
) -> Result<T, String> {
    let url = match query {
        Some(query) => format!("https://api.telegram.org/bot{token}/{method}?{query}"),
        None => format!("https://api.telegram.org/bot{token}/{method}"),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Telegram-Laufzeit konnte nicht erzeugt werden".to_owned())?;
    let response = runtime
        .block_on(async { reqwest::Client::new().get(url).send().await })
        .map_err(|_| "Telegram API konnte nicht erreicht werden".to_owned())?;
    let payload: ApiResponse<T> = runtime
        .block_on(response.json())
        .map_err(|_| "ungültige Antwort von Telegram".to_owned())?;
    if payload.ok {
        Ok(payload.result)
    } else {
        Err("Telegram API hat die Anfrage abgelehnt".to_owned())
    }
}
