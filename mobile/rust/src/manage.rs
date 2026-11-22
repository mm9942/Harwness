//! Verwalten wie in der TUI (Welle 3): Befehle, Katalog, Konfiguration, write-only Secrets und Step-up
//! (Wire Minor 4, Features `manage` und `stepup`). Kein Geheimnis verlaesst diese Datei ausser als
//! `WireSecret` an den Server; nichts davon wird geloggt oder zurueckgegeben.

use aws_lc_rs::digest::{Context, SHA256};
use harw_protocol::session_wire::{
    CatalogGetParams, CommandRunParams, CommandRunResult, CommandsListParams, ConfigGetParams,
    ConfigResetParams, ConfigSetParams, MAX_CLIENT_MSG_ID_BYTES, MAX_COMMAND_LINE_BYTES,
    MAX_CONFIG_KEYS, MAX_SECRET_VALUE_BYTES, SecretPutParams, StepUpCancelParams, StepUpChallenge,
    StepUpConfirmParams, WireSecret, features,
};
use harw_types::SessionId;
use secrecy::{ExposeSecret, SecretString};

use crate::{
    CloudCatalog, ManageCommandList, ManageConfigEntry, ManageKnownAuth, ManageKnownProvider,
    ManageRunResult, ManageSecretHandle, ManageStepUp, ManageStepUpLine, ManageSupport, MobileCore,
    MobileError,
};

/// Domaene des Step-up-Hashs (K-W3c 2.4): `sha256(DOMAIN || nonce_bytes || 0x00 || text_utf8)`.
pub(crate) const STEPUP_HASH_DOMAIN: &[u8] = b"harw-stepup-v1\0";
/// Erste Zeile des kanonischen Texts (K-W3b 2.5), wird in `lines` ausgelassen.
pub(crate) const STEPUP_TEXT_HEADER: &str = "harw step-up v1";
/// Server-Fenster eines Step-ups (Plan 4.5: 60 s).
pub(crate) const STEPUP_MAX_VALID_MS: u64 = 60_000;
/// Countdown, wenn die Handy-Uhr vom Server abweicht (Restzeit <= 0 oder > Max).
pub(crate) const STEPUP_FALLBACK_VALID_MS: u64 = 55_000;
/// Obergrenze fuer `ManageRunResult.data_json` (Bytes).
pub(crate) const MAX_DATA_JSON_BYTES: usize = 256 * 1024;
/// Secret- und Provider-Namen: `[A-Za-z0-9._-]` bzw. `[A-Za-z0-9_-]`.
pub(crate) const MAX_SECRET_NAME_LEN: usize = 64;
pub(crate) const MAX_PROVIDER_NAME_LEN: usize = 48;
/// OAuth-Routen mit Token (Spiegel `harw_ops::provider::OAUTH_ROUTES`, `StagedToken`): (catalog_id, praefix).
pub(crate) const OAUTH_TOKEN_PREFIXES: &[(&str, &str)] = &[("anthropic", "sk-ant-oat")];
/// Befehlszeile fuer die Liste bekannter Provider.
pub(crate) const PROVIDER_CATALOG_LINE: &str = "/provider catalog";

pub(crate) const NO_MANAGE_DETAIL: &str =
    "Server kennt keine Verwaltung (Wire 4, Feature manage fehlt)";
pub(crate) const NO_STEPUP_DETAIL: &str =
    "Server erlaubt noch keine Änderungen vom Handy (Feature stepup fehlt)";
pub(crate) const LINE_INVALID_DETAIL: &str = "ungültige Befehlszeile";
pub(crate) const MSG_ID_INVALID_DETAIL: &str = "ungültige Nachrichten-Id";
pub(crate) const STEPUP_INVALID_DETAIL: &str = "Step-up passt nicht zu seinem Text; abgelehnt";
pub(crate) const SECRET_NAME_DETAIL: &str =
    "Schlüsselname: nur A-Z a-z 0-9 . _ -, höchstens 64 Zeichen";
pub(crate) const PROVIDER_NAME_DETAIL: &str =
    "Providername: nur A-Z a-z 0-9 _ -, höchstens 48 Zeichen";
pub(crate) const SECRET_VALUE_DETAIL: &str = "Schlüssel leer oder zu lang";
pub(crate) const CONFIG_KEYS_DETAIL: &str = "zu viele Schlüssel";
pub(crate) const CATALOG_DATA_DETAIL: &str = "Server lieferte keine Provider-Liste";

fn denied(detail: &str) -> MobileError {
    MobileError::Denied {
        detail: detail.to_owned(),
    }
}

fn parse_session(id: Option<String>) -> Result<Option<SessionId>, MobileError> {
    id.map(|raw| {
        SessionId::try_from_str(raw).map_err(|_| MobileError::Session {
            detail: "Invalid session ID".to_owned(),
        })
    })
    .transpose()
}

#[uniffi::export]
impl MobileCore {
    /// Features/Caps der aktiven Verbindung; ohne Verbindung `ManageSupport::default()`. Nie ein Fehler.
    pub fn manage_support(&self) -> ManageSupport {
        self.manage_support_now()
    }

    /// `commands.list` (Feature manage). Alle Befehle wie im TUI-Popup, nicht verfuegbare mit Grund.
    pub fn commands(&self, session_id: Option<String>) -> Result<ManageCommandList, MobileError> {
        let params = CommandsListParams {
            session_id: parse_session(session_id)?,
        };
        self.manage_call(features::MANAGE, move |port| port.commands_list(params))
            .map(Into::into)
    }

    /// `command.run` (Feature manage). Bei `effect = StepupRequired` traegt das Ergebnis eine GEPRUEFTE
    /// `ManageStepUp`; passt der Hash nicht, ist das ein Fehler (fail closed).
    pub fn command(
        &self,
        session_id: Option<String>,
        line: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_line(&line)?;
        check_client_msg_id(&client_msg_id)?;
        let params = CommandRunParams {
            session_id: parse_session(session_id)?,
            line,
            client_msg_id,
        };
        let result = self.manage_call(features::MANAGE, move |port| port.command_run(params))?;
        run_result(result, jiff::Timestamp::now())
    }

    /// Stoppt einen Kind-Agenten der Sitzung: `command.run "/agent stop <id>"` (Remote-Allowlist, Feature manage,
    /// Cap steer). Das Ergebnis traegt den Text des Hosts; `ok = false` mit Grund, wenn der Host ablehnt.
    pub fn stop_agent(
        &self,
        session_id: String,
        agent_id: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_target_id(&agent_id)?;
        self.command(Some(session_id), format!("/agent stop {agent_id}"), client_msg_id)
    }

    /// Bricht einen Hintergrund-Job der Sitzung ab: `command.run "/jobs stop <id>"` (Feature manage, Cap steer).
    pub fn cancel_job(
        &self,
        session_id: String,
        work_id: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_target_id(&work_id)?;
        self.command(Some(session_id), format!("/jobs stop {work_id}"), client_msg_id)
    }

    /// `catalog.get` (Feature manage). `session_id` fuer das Live-Modell.
    pub fn catalog(&self, session_id: Option<String>) -> Result<CloudCatalog, MobileError> {
        let params = CatalogGetParams {
            session_id: parse_session(session_id)?,
        };
        self.manage_call(features::MANAGE, move |port| port.catalog_get(params))
            .map(Into::into)
    }

    /// Bekannte Provider: `command.run "/provider catalog"`, `data.providers[]` geparst. Feature manage.
    pub fn known_providers(
        &self,
        client_msg_id: String,
    ) -> Result<Vec<ManageKnownProvider>, MobileError> {
        check_client_msg_id(&client_msg_id)?;
        let params = CommandRunParams {
            session_id: None,
            line: PROVIDER_CATALOG_LINE.to_owned(),
            client_msg_id,
        };
        let result = self.manage_call(features::MANAGE, move |port| port.command_run(params))?;
        if !result.ok {
            return Err(MobileError::Denied {
                detail: result.text,
            });
        }
        result
            .data
            .as_ref()
            .and_then(known_from_data)
            .ok_or_else(|| MobileError::Session {
                detail: CATALOG_DATA_DETAIL.to_owned(),
            })
    }

    /// `config.get` (Feature manage). `keys = None` -> alle der Allowlist.
    pub fn config_get(
        &self,
        keys: Option<Vec<String>>,
    ) -> Result<Vec<ManageConfigEntry>, MobileError> {
        if keys.as_ref().is_some_and(|k| k.len() > MAX_CONFIG_KEYS) {
            return Err(denied(CONFIG_KEYS_DETAIL));
        }
        let params = ConfigGetParams { keys };
        let result = self.manage_call(features::MANAGE, move |port| port.config_get(params))?;
        Ok(result.entries.into_iter().map(Into::into).collect())
    }

    /// `config.set` (Feature stepup). Antwort normal `StepupRequired` (gepruefte Herausforderung) oder `Refused`.
    pub fn config_set(
        &self,
        key: String,
        value: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_client_msg_id(&client_msg_id)?;
        let params = ConfigSetParams {
            key,
            value,
            client_msg_id,
        };
        let result = self.manage_call(features::STEPUP, move |port| port.config_set(params))?;
        run_result(result, jiff::Timestamp::now())
    }

    /// `config.reset` (Feature stepup).
    pub fn config_reset(
        &self,
        key: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_client_msg_id(&client_msg_id)?;
        let params = ConfigResetParams { key, client_msg_id };
        let result = self.manage_call(features::STEPUP, move |port| port.config_reset(params))?;
        run_result(result, jiff::Timestamp::now())
    }

    /// `secret.put` (Feature stepup). Write-only: der Wert wird sofort in einem `SecretString` gehalten (nullt beim
    /// Drop), nur als `WireSecret` gesendet, nie geloggt, nie zurueckgegeben. Fehlertexte nennen nie den Wert.
    pub fn secret_put(
        &self,
        name: String,
        for_provider: String,
        value: String,
    ) -> Result<ManageSecretHandle, MobileError> {
        let value = SecretString::from(value);
        check_secret_name(&name)?;
        check_provider_name(&for_provider)?;
        check_secret_value(value.expose_secret())?;
        let sent = value
            .expose_secret()
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        let params = SecretPutParams {
            name,
            for_provider,
            value: WireSecret::new(sent),
        };
        drop(value);
        self.manage_call(features::STEPUP, move |port| port.secret_put(params))
            .map(Into::into)
    }

    /// `stepup.confirm` (Feature stepup). Kotlin ruft das NUR nach einem frischen, erfolgreichen BiometricPrompt.
    /// Prueft die Herausforderung erneut, sendet `{stepup_id, hash}`. `stepup` im Ergebnis ist immer `None`.
    pub fn stepup_confirm(&self, stepup: ManageStepUp) -> Result<ManageRunResult, MobileError> {
        verify_challenge_parts(&stepup.stepup_id, &stepup.nonce, &stepup.text, &stepup.hash)?;
        let params = StepUpConfirmParams {
            stepup_id: stepup.stepup_id,
            hash: stepup.hash,
        };
        let result = self.manage_call(features::STEPUP, move |port| port.stepup_confirm(params))?;
        let mut out = run_result(result, jiff::Timestamp::now())?;
        out.stepup = None;
        Ok(out)
    }

    /// `stepup.cancel` (Feature stepup).
    pub fn stepup_cancel(&self, stepup_id: String) -> Result<(), MobileError> {
        if !is_lower_hex(&stepup_id, 32) {
            return Err(denied(STEPUP_INVALID_DETAIL));
        }
        let params = StepUpCancelParams { stepup_id };
        self.manage_call(features::STEPUP, move |port| port.stepup_cancel(params))
    }
}

// ---- reine Helfer ----

/// `line` beginnt mit `/`, 2..=MAX_COMMAND_LINE_BYTES Bytes, kein Steuerzeichen.
pub(crate) fn check_line(line: &str) -> Result<(), MobileError> {
    if line.starts_with('/')
        && (2..=MAX_COMMAND_LINE_BYTES).contains(&line.len())
        && !line.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err(denied(LINE_INVALID_DETAIL))
    }
}

/// Agent-/Job-ID als ein Token der Befehlszeile: 1..=128 Bytes, nur ASCII alphanumerisch, `-`, `_`, `.`, `:`.
pub(crate) fn check_target_id(id: &str) -> Result<(), MobileError> {
    if (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        Ok(())
    } else {
        Err(denied("Ungültige Agent- oder Job-ID"))
    }
}

/// 1..=MAX_CLIENT_MSG_ID_BYTES, nur ASCII graphisch.
pub(crate) fn check_client_msg_id(id: &str) -> Result<(), MobileError> {
    if (1..=MAX_CLIENT_MSG_ID_BYTES).contains(&id.len()) && id.chars().all(|c| c.is_ascii_graphic())
    {
        Ok(())
    } else {
        Err(denied(MSG_ID_INVALID_DETAIL))
    }
}

fn name_ok(name: &str, max: usize, allow_dot: bool) -> bool {
    (1..=max).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || (allow_dot && c == '.'))
}

/// `[A-Za-z0-9._-]`, 1..=MAX_SECRET_NAME_LEN.
pub(crate) fn check_secret_name(name: &str) -> Result<(), MobileError> {
    if name_ok(name, MAX_SECRET_NAME_LEN, true) {
        Ok(())
    } else {
        Err(denied(SECRET_NAME_DETAIL))
    }
}

/// `[A-Za-z0-9_-]`, 1..=MAX_PROVIDER_NAME_LEN.
pub(crate) fn check_provider_name(name: &str) -> Result<(), MobileError> {
    if name_ok(name, MAX_PROVIDER_NAME_LEN, false) {
        Ok(())
    } else {
        Err(denied(PROVIDER_NAME_DETAIL))
    }
}

/// Leer (nach Entfernen von abschliessendem CR/LF) oder zu lang -> Denied. Der Text nennt nie den Wert.
pub(crate) fn check_secret_value(value: &str) -> Result<(), MobileError> {
    let trimmed = value.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() || trimmed.len() > MAX_SECRET_VALUE_BYTES {
        Err(denied(SECRET_VALUE_DETAIL))
    } else {
        Ok(())
    }
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `hex(sha256(STEPUP_HASH_DOMAIN || hex_decode(nonce) || [0x00] || text))`, klein.
/// `nonce` muss 64 Hex-Zeichen sein, sonst `None`.
pub(crate) fn stepup_hash(nonce: &str, text: &str) -> Option<String> {
    if nonce.len() != 64 {
        return None;
    }
    let nonce_bytes = hex::decode(nonce).ok()?;
    let mut ctx = Context::new(&SHA256);
    ctx.update(STEPUP_HASH_DOMAIN);
    ctx.update(&nonce_bytes);
    ctx.update(&[0u8]);
    ctx.update(text.as_bytes());
    Some(hex::encode(ctx.finish().as_ref()))
}

/// Prueft Formate und dass `stepup_hash(nonce, text) == hash`. Sonst `Err(Denied{STEPUP_INVALID_DETAIL})`.
pub(crate) fn verify_challenge_parts(
    stepup_id: &str,
    nonce: &str,
    text: &str,
    hash: &str,
) -> Result<(), MobileError> {
    let formats_ok = is_lower_hex(stepup_id, 32)
        && is_lower_hex(nonce, 64)
        && is_lower_hex(hash, 64)
        && !text.is_empty();
    if formats_ok && stepup_hash(nonce, text).as_deref() == Some(hash) {
        Ok(())
    } else {
        Err(denied(STEPUP_INVALID_DETAIL))
    }
}

/// Wire -> gepruefte `ManageStepUp`. `valid_for_ms`: Restzeit in `1..=STEPUP_MAX_VALID_MS`, sonst Fallback.
pub(crate) fn stepup_from_wire(
    challenge: StepUpChallenge,
    now: jiff::Timestamp,
) -> Result<ManageStepUp, MobileError> {
    verify_challenge_parts(
        &challenge.stepup_id,
        &challenge.nonce,
        &challenge.text,
        &challenge.hash,
    )?;
    let rest = challenge
        .expires_at
        .as_millisecond()
        .saturating_sub(now.as_millisecond());
    let valid_for_ms = match u64::try_from(rest) {
        Ok(ms) if (1..=STEPUP_MAX_VALID_MS).contains(&ms) => ms,
        _ => STEPUP_FALLBACK_VALID_MS,
    };
    let (lines, address, key_name) = stepup_lines(&challenge.text);
    Ok(ManageStepUp {
        stepup_id: challenge.stepup_id,
        action: challenge.action,
        target: challenge.target,
        text: challenge.text,
        nonce: challenge.nonce,
        hash: challenge.hash,
        expires_at: challenge.expires_at.to_string(),
        valid_for_ms,
        lines,
        address,
        key_name,
    })
}

/// Zerlegt den Text in Anzeigezeilen, `adresse` (wenn nicht `-`) und den Namen aus `schluessel: neu "<name>" ...`.
pub(crate) fn stepup_lines(text: &str) -> (Vec<ManageStepUpLine>, Option<String>, Option<String>) {
    let mut lines = Vec::new();
    let mut address = None;
    let mut key_name = None;
    for (index, raw) in text.lines().enumerate() {
        if index == 0 && raw == STEPUP_TEXT_HEADER {
            continue;
        }
        if raw.is_empty() {
            continue;
        }
        let (label, value) = raw.split_once(": ").unwrap_or(("", raw));
        if label == "adresse" && value != "-" {
            address = Some(value.to_owned());
        }
        if label == "schluessel"
            && let Some(rest) = value.strip_prefix("neu \"")
            && let Some((name, _)) = rest.split_once('"')
        {
            key_name = Some(name.to_owned());
        }
        lines.push(ManageStepUpLine {
            label: label.to_owned(),
            value: value.to_owned(),
        });
    }
    (lines, address, key_name)
}

/// Wire -> `ManageRunResult`. Zu grosse `data` entfallen; eine Herausforderung wird geprueft (fail closed).
pub(crate) fn run_result(
    result: CommandRunResult,
    now: jiff::Timestamp,
) -> Result<ManageRunResult, MobileError> {
    let mut data_json = None;
    let mut data_truncated = false;
    if let Some(data) = &result.data
        && let Ok(pretty) = serde_json::to_string_pretty(data)
    {
        if pretty.len() > MAX_DATA_JSON_BYTES {
            data_truncated = true;
        } else {
            data_json = Some(pretty);
        }
    }
    let stepup = result
        .stepup
        .map(|challenge| stepup_from_wire(challenge, now))
        .transpose()?;
    Ok(ManageRunResult {
        ok: result.ok,
        text: result.text,
        data_json,
        data_truncated,
        effect: result.effect.into(),
        reason_code: result.reason_code,
        stepup,
    })
}

#[derive(serde::Deserialize)]
struct KnownWire {
    id: String,
    display_name: String,
    api: String,
    base_url: String,
    #[serde(default)]
    auth_kinds: Vec<String>,
    default_name_api_key: String,
    #[serde(default)]
    default_name_oauth: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    api_key_taken: bool,
    #[serde(default)]
    oauth_taken: bool,
}

/// `data.providers[]` -> bekannte Provider. Unbekannte `auth_kinds`-Strings und unlesbare Eintraege werden
/// uebersprungen. Fehlt `providers`, `None`.
pub(crate) fn known_from_data(data: &serde_json::Value) -> Option<Vec<ManageKnownProvider>> {
    let rows = data.get("providers")?.as_array()?;
    let mut out = Vec::new();
    for row in rows {
        let Ok(wire) = serde_json::from_value::<KnownWire>(row.clone()) else {
            continue;
        };
        let auth_kinds: Vec<ManageKnownAuth> = wire
            .auth_kinds
            .iter()
            .filter_map(|kind| match kind.as_str() {
                "api-key" => Some(ManageKnownAuth::ApiKey),
                "oauth" => Some(ManageKnownAuth::Oauth),
                "none" => Some(ManageKnownAuth::None),
                _ => None,
            })
            .collect();
        let oauth_token_prefix = if auth_kinds.contains(&ManageKnownAuth::Oauth) {
            OAUTH_TOKEN_PREFIXES
                .iter()
                .find(|(id, _)| *id == wire.id)
                .map(|(_, prefix)| (*prefix).to_owned())
        } else {
            None
        };
        out.push(ManageKnownProvider {
            id: wire.id,
            display_name: wire.display_name,
            api: wire.api,
            base_url: wire.base_url,
            auth_kinds,
            default_name_api_key: wire.default_name_api_key,
            default_name_oauth: wire.default_name_oauth,
            models: wire.models,
            api_key_taken: wire.api_key_taken,
            oauth_taken: wire.oauth_taken,
            oauth_token_prefix,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManageEffect;
    use harw_protocol::session_wire::CommandEffect;

    type T = Result<(), String>;

    fn detail(err: MobileError) -> String {
        match err {
            MobileError::Denied { detail } => detail,
            other => format!("unexpected: {other}"),
        }
    }

    fn ts(secs: i64) -> Result<jiff::Timestamp, String> {
        jiff::Timestamp::from_second(secs).map_err(|e| e.to_string())
    }

    fn challenge(text: &str, expires: jiff::Timestamp) -> Result<StepUpChallenge, String> {
        let nonce = "00".repeat(32);
        let hash = stepup_hash(&nonce, text).ok_or("hash")?;
        Ok(StepUpChallenge {
            stepup_id: "ab".repeat(16),
            action: "provider.add".into(),
            target: "custom-a".into(),
            text: text.into(),
            nonce,
            hash,
            expires_at: expires,
        })
    }

    const ADD_TEXT: &str = "harw step-up v1\naktion: provider.add\nprovider: custom-a\nquelle: eigen\nadresse: https://llm.example.test/v1\nschluessel: neu \"custom-a\" (nur schreiben)";

    #[test]
    fn check_line_rules() -> T {
        check_line("/status").map_err(|e| e.to_string())?;
        for bad in ["status", "/", "", "/a\nb", "/a\tb"] {
            assert_eq!(
                detail(check_line(bad).err().ok_or("accepted")?),
                LINE_INVALID_DETAIL
            );
        }
        let long = format!("/{}", "x".repeat(512));
        assert!(check_line(&long).is_err());
        let exact = format!("/{}", "x".repeat(MAX_COMMAND_LINE_BYTES - 1));
        check_line(&exact).map_err(|e| e.to_string())
    }

    #[test]
    fn client_msg_id_rules() -> T {
        check_client_msg_id("m-1").map_err(|e| e.to_string())?;
        assert!(check_client_msg_id("").is_err());
        assert!(check_client_msg_id("a b").is_err());
        assert!(check_client_msg_id(&"a".repeat(129)).is_err());
        Ok(())
    }

    #[test]
    fn secret_and_provider_names() -> T {
        check_secret_name("anthropic-key").map_err(|e| e.to_string())?;
        check_provider_name("anthropic-key").map_err(|e| e.to_string())?;
        check_secret_name("a.b").map_err(|e| e.to_string())?;
        assert!(check_provider_name("a.b").is_err());
        let long = "a".repeat(65);
        for bad in ["a/b", "", long.as_str()] {
            assert!(check_secret_name(bad).is_err());
        }
        Ok(())
    }

    #[test]
    fn secret_value_rules_never_echo() -> T {
        for bad in [String::new(), "\r\n".to_owned(), "x".repeat(8195)] {
            let text = detail(check_secret_value(&bad).err().ok_or("accepted")?);
            assert_eq!(text, SECRET_VALUE_DETAIL);
            assert!(!text.contains("xxxxxxxxxx"));
        }
        check_secret_value("sk-test-FAKE-0000000000000000").map_err(|e| e.to_string())?;
        let text = detail(
            check_secret_name("sk-test-FAKE-0000000000000000 x")
                .err()
                .ok_or("accepted")?,
        );
        assert!(!text.contains("FAKE"));
        Ok(())
    }

    #[test]
    fn stepup_hash_matches_server_formula() -> T {
        let text = "harw step-up v1\naktion: models.set";
        let mut ctx = Context::new(&SHA256);
        ctx.update(b"harw-stepup-v1\0");
        ctx.update(&[0u8; 32]);
        ctx.update(&[0u8]);
        ctx.update(text.as_bytes());
        let expected = hex::encode(ctx.finish().as_ref());
        assert_eq!(stepup_hash(&"00".repeat(32), text), Some(expected));
        assert_eq!(stepup_hash("zz", text), None);
        Ok(())
    }

    #[test]
    fn tampered_text_is_rejected() -> T {
        let mut ch = challenge(ADD_TEXT, ts(1_000)?)?;
        stepup_from_wire(ch.clone(), ts(990)?).map_err(|e| e.to_string())?;
        ch.text = ch.text.replace("llm.example.test", "llm.example.tesu");
        let err = stepup_from_wire(ch, ts(990)?).err().ok_or("accepted")?;
        assert_eq!(detail(err), STEPUP_INVALID_DETAIL);
        let mut upper = challenge(ADD_TEXT, ts(1_000)?)?;
        upper.hash = upper.hash.to_uppercase();
        assert!(stepup_from_wire(upper, ts(990)?).is_err());
        Ok(())
    }

    #[test]
    fn stepup_lines_extract_address_and_key() -> T {
        let (lines, address, key) = stepup_lines(ADD_TEXT);
        assert_eq!(address.as_deref(), Some("https://llm.example.test/v1"));
        assert_eq!(key.as_deref(), Some("custom-a"));
        assert_eq!(lines.len(), 5);
        assert!(
            lines
                .iter()
                .all(|l| l.label != STEPUP_TEXT_HEADER && l.value != STEPUP_TEXT_HEADER)
        );
        let (_, none, _) = stepup_lines("harw step-up v1\nadresse: -");
        assert_eq!(none, None);
        let (lines, _, _) = stepup_lines("harw step-up v1\nalt -> neu: a: b");
        assert_eq!(lines[0].label, "alt -> neu");
        assert_eq!(lines[0].value, "a: b");
        Ok(())
    }

    #[test]
    fn valid_for_ms_clamps() -> T {
        let now = ts(1_000)?;
        let ms = |expires| -> Result<u64, String> {
            Ok(stepup_from_wire(challenge(ADD_TEXT, expires)?, now)
                .map_err(|e| e.to_string())?
                .valid_for_ms)
        };
        assert_eq!(ms(ts(1_030)?)?, 30_000);
        assert_eq!(ms(ts(1_120)?)?, STEPUP_FALLBACK_VALID_MS);
        assert_eq!(ms(ts(999)?)?, STEPUP_FALLBACK_VALID_MS);
        Ok(())
    }

    fn plain(effect: CommandEffect) -> CommandRunResult {
        CommandRunResult {
            ok: true,
            text: "t".into(),
            data: None,
            effect,
            reason_code: None,
            stepup: None,
        }
    }

    #[test]
    fn run_result_truncates_large_data() -> T {
        let mut big = plain(CommandEffect::Applied);
        big.data = Some(serde_json::Value::String("x".repeat(300 * 1024)));
        let out = run_result(big, ts(0)?).map_err(|e| e.to_string())?;
        assert!(out.data_json.is_none() && out.data_truncated);
        let mut small = plain(CommandEffect::Applied);
        small.data = Some(serde_json::json!({"a": 1}));
        let out = run_result(small, ts(0)?).map_err(|e| e.to_string())?;
        assert!(out.data_json.is_some_and(|d| d.contains('\n')) && !out.data_truncated);
        Ok(())
    }

    #[test]
    fn run_result_refused_keeps_reason() -> T {
        let mut refused = plain(CommandEffect::Refused);
        refused.ok = false;
        refused.reason_code = Some("slice_disabled".into());
        let out = run_result(refused, ts(0)?).map_err(|e| e.to_string())?;
        assert!(!out.ok);
        assert_eq!(out.effect, ManageEffect::Refused);
        assert_eq!(out.reason_code.as_deref(), Some("slice_disabled"));
        assert!(out.stepup.is_none());
        Ok(())
    }

    #[test]
    fn run_result_stepup_is_verified() -> T {
        let mut with = plain(CommandEffect::StepupRequired);
        with.stepup = Some(challenge(ADD_TEXT, ts(1_030)?)?);
        let out = run_result(with, ts(1_000)?).map_err(|e| e.to_string())?;
        assert_eq!(out.effect, ManageEffect::StepupRequired);
        assert!(out.stepup.is_some());
        let mut bad = plain(CommandEffect::StepupRequired);
        let mut ch = challenge(ADD_TEXT, ts(1_030)?)?;
        ch.hash = "0".repeat(64);
        bad.stepup = Some(ch);
        assert!(run_result(bad, ts(1_000)?).is_err());
        Ok(())
    }

    #[test]
    fn known_from_data_parses_catalog() -> T {
        let data = serde_json::json!({"providers":[
            {"id":"anthropic","display_name":"Anthropic","api":"anthropic-messages",
             "base_url":"https://api.anthropic.example.test/v1","auth_kinds":["api-key","oauth"],
             "default_name_api_key":"anthropic-key","default_name_oauth":"anthropic-oauth",
             "models":["m-1"],"api_key_taken":true,"future":1},
            {"id":"openai","display_name":"OpenAI","api":"openai-responses",
             "base_url":"https://api.openai.example.test/v1",
             "auth_kinds":["api-key","oauth","future-kind"],"default_name_api_key":"openai-key"}
        ]});
        let rows = known_from_data(&data).ok_or("none")?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].oauth_token_prefix.as_deref(), Some("sk-ant-oat"));
        assert!(rows[0].api_key_taken);
        assert_eq!(rows[1].oauth_token_prefix, None);
        assert_eq!(
            rows[1].auth_kinds,
            vec![ManageKnownAuth::ApiKey, ManageKnownAuth::Oauth]
        );
        assert!(known_from_data(&serde_json::json!({})).is_none());
        Ok(())
    }

    #[test]
    fn manage_support_default_is_all_false() {
        let s = ManageSupport::default();
        assert!(!(s.manage || s.stepup || s.observe || s.steer || s.approve || s.control));
    }
}
