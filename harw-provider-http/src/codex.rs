//! ChatGPT/Codex route for the existing Codex CLI login.
//!
//! Harw rereads the shared credential file (`~/.codex/auth.json`) for each
//! request. Unlike the original design, `harw` now also owns a *refresh*
//! path for the short-lived access token: before a request, an
//! about-to-expire token is refreshed proactively
//! (see [`harw_oauth::jwt_needs_refresh`]); a genuine `401` triggers exactly
//! one reactive refresh-and-retry (see [`CodexRoute::refresh`] and its call
//! site in `lib.rs`). The actual token exchange, locking and atomic
//! persistence into `auth.json` live in the `harw-oauth` crate
//! ([`harw_oauth::refresh_codex_tokens`]) so this module stays a thin,
//! endpoint-bound consumer.
//! Wire reference: ../codex/codex-rs/{model-provider-info,codex-api,login}.

use std::path::{Path, PathBuf};

use harw_config::{ProviderToml, SecretRef};
use harw_core::ModelError;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use secrecy::ExposeSecret;
use serde_json::Value;

use crate::{HttpProviderError, HttpProviderResult};

pub(crate) const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const API_BASE_URL: &str = "https://api.openai.com/v1";
const ACCESS_POINTER: &str = "/tokens/access_token";
const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
/// Proaktives Refresh-Fenster: läuft das Access-Token in weniger als dieser
/// Restlaufzeit ab, wird vor dem Request erneuert (siehe Auftrag: "weniger
/// als 5 Minuten Restlaufzeit").
const PROACTIVE_REFRESH_WINDOW_SECONDS: i64 = 5 * 60;

/// Harw-eigener Default für den `originator`-Header, wenn
/// [`harw_config::ProviderToml::originator`] nicht gesetzt (oder leer) ist.
const DEFAULT_ORIGINATOR: &str = "harw";

/// Relativer Pfad der Codex-CLI-Anmeldedatei unter `$HOME`.
const CODEX_AUTH_RELATIVE: &str = ".codex/auth.json";

/// Prüft, ob eine Secret-Referenz auf das Codex-Login-Token zeigt.
///
/// # Description
/// Trifft genau auf `file-json:<…>/.codex/auth.json#/tokens/access_token` zu —
/// das ChatGPT-access_token, das nur für die Codex-Route
/// (`https://chatgpt.com/backend-api/codex`) auflösbar ist. Der API-Key aus
/// derselben Datei (`#/OPENAI_API_KEY`) zählt **nicht** dazu. Die Prüfung ist
/// rein syntaktisch (kein Dateizugriff, kein `$HOME`-Vergleich), damit sie
/// auch für Einträge aus einer fremden Umgebung stabil bleibt.
///
/// # Arguments
/// - `secret` (`&SecretRef`): zu prüfende Referenz.
///
/// # Returns
/// `true` für einen Codex-Login-Verweis, sonst `false`.
pub fn is_codex_login_reference(secret: &SecretRef) -> bool {
    matches!(
        secret,
        SecretRef::FileJson { path, pointer }
            if pointer == ACCESS_POINTER && Path::new(path).ends_with(CODEX_AUTH_RELATIVE)
    )
}

/// Prüft, ob eine Basis-URL die offizielle Codex-Route ist.
///
/// # Arguments
/// - `base_url` (`&str`): Basis-URL eines Providers (abschließende `/` egal).
///
/// # Returns
/// `true` genau für `https://chatgpt.com/backend-api/codex`.
pub fn is_codex_base_url(base_url: &str) -> bool {
    base_url.trim_end_matches('/') == BASE_URL
}

/// An endpoint-bound, read-only reference to the Codex login.
pub(crate) struct CodexRoute {
    path: String,
    /// Wert des `originator`-Headers für diese Route (siehe
    /// [`harw_config::ProviderToml::originator`]); `"harw"`, wenn der
    /// Provider keinen eigenen Wert konfiguriert.
    originator: String,
}

impl CodexRoute {
    pub(crate) fn from_provider(provider: &ProviderToml) -> HttpProviderResult<Option<Self>> {
        let Some(SecretRef::FileJson { path, pointer }) = &provider.auth else {
            return Ok(None);
        };
        let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
            return Ok(None);
        };
        if Path::new(path) != PathBuf::from(home).join(CODEX_AUTH_RELATIVE)
            || pointer != ACCESS_POINTER
        {
            return Ok(None);
        }
        // Migrate precisely the old broken route in memory. Never redirect a
        // configured gateway or another API dialect based on token contents.
        if provider.api != "openai-responses"
            || !matches!(
                provider.base_url.trim_end_matches('/'),
                API_BASE_URL | BASE_URL
            )
            || !matches!(provider.auth_header.as_deref(), None | Some("bearer"))
            || provider.headers.keys().any(|name| {
                name.eq_ignore_ascii_case("authorization")
                    || name.eq_ignore_ascii_case("chatgpt-account-id")
            })
        {
            return Err(HttpProviderError::Decode(
                "Codex login requires openai-responses, the official Codex base URL and bearer authentication without account/auth header overrides".into(),
            ));
        }
        // `ProviderToml::validate` (aufgerufen beim Konfigurations-Laden,
        // siehe `harw-config/src/discovery.rs`) erzwingt bereits nicht-leer/
        // druckbares-ASCII/max. 64 Zeichen. Der Fallback hier ist reine
        // Verteidigung in der Tiefe für Aufrufer, die eine `ProviderToml`
        // ohne vorherige Validierung konstruieren (z. B. Tests).
        let originator = provider
            .originator
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_ORIGINATOR)
            .to_owned();
        Ok(Some(Self {
            path: path.clone(),
            originator,
        }))
    }

    /// Der für diese Route konfigurierte `originator`-Header-Wert (siehe
    /// [`harw_config::ProviderToml::originator`]).
    pub(crate) fn originator(&self) -> &str {
        &self.originator
    }

    /// Validiert ohne Netzwerkzugriff, dass die Codex-Login-Datei lesbar und
    /// als Header-Quelle brauchbar ist (kein proaktiver Refresh, kein
    /// Request) — für Aufrufer, die nur die Verfügbarkeit prüfen wollen.
    pub(crate) fn validate_login(&self) -> Result<(), ModelError> {
        let raw = crate::read_external_cli_credential(Some(BASE_URL), &self.path, ACCESS_POINTER)
            .ok_or_else(login_error)?
            .map_err(|_| login_error())?;
        let document: Value =
            serde_json::from_str(raw.expose_secret()).map_err(|_| login_error())?;
        login_headers(&document, &self.originator).map(|_headers| ())
    }

    /// Baut die Auth-Header für einen Request; erneuert das Access-Token
    /// vorab, wenn es innerhalb von [`PROACTIVE_REFRESH_WINDOW_SECONDS`]
    /// abläuft.
    ///
    /// Schlägt der proaktive Refresh fehl (z. B. Netzwerkfehler), wird dieser
    /// Fehler bewusst verschluckt: die bestehenden Header werden trotzdem
    /// gebaut, und ein tatsächlicher `401` löst am Aufrufer (`lib.rs`) den
    /// reaktiven Einmal-Retry über [`Self::refresh`] aus.
    pub(crate) async fn headers(&self, client: &reqwest::Client) -> Result<HeaderMap, ModelError> {
        if self.access_token_needs_refresh() {
            let _ = self.refresh(client).await;
        }
        let raw = crate::read_external_cli_credential(Some(BASE_URL), &self.path, ACCESS_POINTER)
            .ok_or_else(login_error)?
            .map_err(|_| login_error())?;
        let document: Value =
            serde_json::from_str(raw.expose_secret()).map_err(|_| login_error())?;
        login_headers(&document, &self.originator)
    }

    /// Erneuert das Codex-/ChatGPT-Token-Set über [`harw_oauth::refresh_codex_tokens`]
    /// und schreibt es atomar zurück in `~/.codex/auth.json`.
    ///
    /// Wird sowohl proaktiv (siehe [`Self::headers`]) als auch reaktiv nach
    /// einem tatsächlichen `401` genau einmal pro Request aufgerufen (siehe
    /// Aufrufstelle in `lib.rs`); es gibt keinen zweiten Refresh-Versuch nach
    /// einem erneuten `401`.
    pub(crate) async fn refresh(&self, client: &reqwest::Client) -> Result<(), ModelError> {
        harw_oauth::refresh_codex_tokens(client, Path::new(&self.path))
            .await
            .map(|_tokens| ())
            .map_err(|_| login_error())
    }

    /// Liest das aktuelle Access-Token und prüft, ob es innerhalb des
    /// proaktiven Refresh-Fensters abläuft. Kann das Token nicht gelesen oder
    /// nicht als JWT dekodiert werden, wird konservativ `false` geliefert —
    /// der reaktive `401`-Pfad greift dann als Fallback.
    fn access_token_needs_refresh(&self) -> bool {
        let Some(Ok(raw)) =
            crate::read_external_cli_credential(Some(BASE_URL), &self.path, ACCESS_POINTER)
        else {
            return false;
        };
        let Ok(document) = serde_json::from_str::<Value>(raw.expose_secret()) else {
            return false;
        };
        let Some(token) = document.pointer(ACCESS_POINTER).and_then(Value::as_str) else {
            return false;
        };
        harw_oauth::jwt_needs_refresh(token, PROACTIVE_REFRESH_WINDOW_SECONDS)
    }
}

fn login_error() -> ModelError {
    ModelError::Auth {
        message: "Codex login is unavailable; run `codex login` and retry".into(),
    }
}

fn login_headers(document: &Value, originator: &str) -> Result<HeaderMap, ModelError> {
    let token = document
        .pointer(ACCESS_POINTER)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(login_error)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        crate::sensitive_header_value(&format!("Bearer {token}"))?,
    );
    if let Some(account) = document
        .pointer("/tokens/account_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|account| !account.is_empty())
    {
        headers.insert(
            "chatgpt-account-id",
            crate::sensitive_header_value(account)?,
        );
    }
    // `originator` ist bei OpenAI reine Client-Identifikation; siehe
    // [`harw_config::ProviderToml::originator`] für die Auftrags-/ToS-Hinweise
    // zu einem vom harw-Default abweichenden Wert. Nicht sensitiv: kein
    // Tokenmaterial. Ein trotz `ProviderToml::validate` ungültiger Wert
    // (z. B. bei Konstruktion ohne vorherige Validierung) fällt fail-safe auf
    // den harw-Default zurück, statt den Request mit einem Header-Fehler
    // scheitern zu lassen.
    headers.insert(
        "originator",
        HeaderValue::from_str(originator)
            .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_ORIGINATOR)),
    );
    // PROVISIONAL: weder "OpenAI-Beta" noch "session_id" tauchen im
    // Referenzverzeichnis ../codex/codex-rs (model-provider/src/auth.rs,
    // model-provider/src/models_endpoint.rs, login/src/auth/manager.rs) für
    // den ChatGPT-Bearer-Auth-Pfad auf `backend-api/codex` auf — die aktuelle
    // Codex-CLI sendet diese Header auf diesem Pfad nach Stand dieser Session
    // *nicht*. Die Werte unten sind deshalb bewusst vorläufige Platzhalter
    // (nicht aus einer verifizierten Quelle abgeleitet); sollte ein
    // zukünftiger Abgleich mit ../codex andere/zusätzliche Werte zeigen, sind
    // sie hier zu korrigieren. Der Wert für "OpenAI-Beta" folgt dem in
    // anderen OpenAI-Wire-Formaten üblichen Feature-Flag-Namensschema; die
    // Header werden bewusst nicht als sensitiv markiert, da sie kein
    // Tokenmaterial enthalten.
    headers.insert(
        "OpenAI-Beta",
        HeaderValue::from_static("responses=experimental"),
    );
    headers.insert(
        "session_id",
        HeaderValue::from_str(&uuid_v4_like()).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    Ok(headers)
}

/// Erzeugt eine zufällige, UUID-v4-förmige Zeichenkette für den
/// (vorläufigen, siehe [`login_headers`]) `session_id`-Header — ohne neue
/// Abhängigkeit auf eine UUID-Crate.
fn uuid_v4_like() -> String {
    let mut bytes = [0u8; 16];
    getrandom_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Füllt `buf` mit Zufallsbytes über das Betriebssystem-RNG (`/dev/urandom`
/// unter Unix), ohne eine zusätzliche `rand`-Abhängigkeit einzuführen. Schlägt
/// das Lesen fehl, bleiben die Bytes `0` — der `session_id`-Header ist ein
/// vorläufiger, nicht sicherheitsrelevanter Platzhalter (siehe [`login_headers`]).
fn getrandom_bytes(buf: &mut [u8]) {
    use std::io::Read as _;
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(buf);
    }
}

pub(crate) fn prepare_body(body: &mut Value) {
    body["stream"] = Value::Bool(true);
    body["store"] = Value::Bool(false);
    if body.get("instructions").is_none_or(Value::is_null) {
        body["instructions"] = Value::String(String::new());
    }
    // This backend controls its output budget and rejects this Platform field.
    if let Some(object) = body.as_object_mut() {
        object.remove("max_output_tokens");
    }
}

/// Read SSE incrementally and retain only one bounded event. Never execute
/// partial tool calls: the terminal response is the sole source of results.
pub(crate) async fn read_response(
    mut response: reqwest::Response,
    sink: Option<&harw_core::StreamSink>,
) -> Result<Value, ModelError> {
    let mut decoder = ResponseDecoder::default();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| crate::error::model_error_for_transport(error, true))?
    {
        if let Some(value) = decoder.push(&chunk, sink)? {
            return Ok(value);
        }
    }
    Err(ModelError::Truncated {
        message: "Codex response stream ended before its terminal event".into(),
    })
}

#[derive(Default)]
struct ResponseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
}

impl ResponseDecoder {
    fn push(
        &mut self,
        bytes: &[u8],
        sink: Option<&harw_core::StreamSink>,
    ) -> Result<Option<Value>, ModelError> {
        for &byte in bytes {
            if byte != b'\n' {
                if self.line.len() + self.data.len() >= MAX_EVENT_BYTES {
                    return Err(ModelError::RequestFailed(
                        "Codex SSE event exceeds size limit".into(),
                    ));
                }
                self.line.push(byte);
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line.is_empty() {
                let data = std::mem::take(&mut self.data);
                if data.is_empty() {
                    continue;
                }
                let event: Value = serde_json::from_slice(&data)
                    .map_err(|_| ModelError::RequestFailed("Invalid Codex SSE event".into()))?;
                match event.get("type").and_then(Value::as_str) {
                    Some("response.completed" | "response.incomplete") => {
                        let mut response = event
                            .get("response")
                            .filter(|value| value.is_object())
                            .cloned()
                            .ok_or_else(|| {
                                ModelError::RequestFailed("Missing Codex terminal response".into())
                            })?;
                        if event["type"] == "response.incomplete" {
                            response["status"] = Value::String("incomplete".into());
                        }
                        return Ok(Some(response));
                    }
                    Some("error" | "response.failed" | "response.cancelled") => {
                        return Err(ModelError::RequestFailed(
                            "Codex response stream reported a failure".into(),
                        ));
                    }
                    // Live-Deltas (Text/Reasoning/Tool-Argumente) weiterreichen;
                    // ausgeführt wird weiterhin nur die Terminal-Antwort.
                    _ => {
                        crate::sse::responses_event(&event, sink)?;
                    }
                }
            } else if let Some(data) = self.line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(data);
            }
            self.line.clear();
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use serde_json::json;

    fn provider(base_url: &str, pointer: &str) -> TestResult<ProviderToml> {
        let home = std::env::var_os("HOME").ok_or(TestError::Missing("HOME"))?;
        Ok(ProviderToml {
            stream: None,
            name: "openai".into(),
            api: "openai-responses".into(),
            base_url: base_url.into(),
            auth: Some(SecretRef::FileJson {
                path: PathBuf::from(home)
                    .join(".codex/auth.json")
                    .to_string_lossy()
                    .into(),
                pointer: pointer.into(),
            }),
            auth_header: None,
            api_key: None,
            headers: Default::default(),
            models: vec![],
            enabled: true,
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
        })
    }

    #[test]
    fn migrates_only_canonical_codex_login_route() -> TestResult {
        for base in [API_BASE_URL, BASE_URL] {
            assert!(
                CodexRoute::from_provider(&provider(base, ACCESS_POINTER)?)
                    .map_err(ctx("from_provider"))?
                    .is_some()
            );
        }
        assert!(
            CodexRoute::from_provider(&provider(API_BASE_URL, "/OPENAI_API_KEY")?)
                .map_err(ctx("from_provider"))?
                .is_none()
        );
        for base in [
            "https://example.com/v1",
            "https://chatgpt.com/other",
            "http://chatgpt.com/backend-api/codex",
            "https://api.openai.com/v1?x=y",
        ] {
            assert!(CodexRoute::from_provider(&provider(base, ACCESS_POINTER)?).is_err());
        }
        let mut wrong = provider(BASE_URL, ACCESS_POINTER)?;
        wrong.api = "openai-chat".into();
        assert!(CodexRoute::from_provider(&wrong).is_err());
        wrong.api = "openai-responses".into();
        wrong
            .headers
            .insert("ChatGPT-Account-ID".into(), "different-account".into());
        assert!(CodexRoute::from_provider(&wrong).is_err());
        Ok(())
    }

    #[test]
    fn credentials_are_sensitive_and_missing_login_is_actionable() -> TestResult {
        let headers = login_headers(
            &json!({"tokens":{"access_token":"test-token","account_id":"test-account"}}),
            DEFAULT_ORIGINATOR,
        )
        .map_err(ctx("login_headers"))?;
        assert_eq!(headers[AUTHORIZATION], "Bearer test-token");
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert!(headers["chatgpt-account-id"].is_sensitive());
        let Err(error) = login_headers(&json!({"tokens":{"access_token":" "}}), DEFAULT_ORIGINATOR)
        else {
            return Err(TestError::Unexpected(
                "login_headers must reject a blank access token".into(),
            ));
        };
        assert!(error.to_string().contains("codex login"));
        Ok(())
    }

    #[test]
    fn login_headers_include_provisional_openai_beta_and_session_id() -> TestResult {
        let headers = login_headers(
            &json!({"tokens":{"access_token":"test-token"}}),
            DEFAULT_ORIGINATOR,
        )
        .map_err(ctx("login_headers"))?;
        assert!(headers.contains_key("OpenAI-Beta"));
        assert!(headers.contains_key("session_id"));
        // Provisorische Header, siehe WHY-Kommentar in `login_headers`: kein
        // Tokenmaterial, daher bewusst nicht sensitiv markiert.
        assert!(!headers["OpenAI-Beta"].is_sensitive());
        assert!(!headers["session_id"].is_sensitive());
        Ok(())
    }

    #[test]
    fn login_headers_default_originator_is_harw() -> TestResult {
        let headers = login_headers(&json!({"tokens":{"access_token":"t"}}), DEFAULT_ORIGINATOR)
            .map_err(ctx("login_headers"))?;
        assert_eq!(headers["originator"], "harw");
        assert!(!headers["originator"].is_sensitive());
        Ok(())
    }

    #[test]
    fn login_headers_uses_configured_originator() -> TestResult {
        let headers = login_headers(&json!({"tokens":{"access_token":"t"}}), "codex_cli_rs")
            .map_err(ctx("login_headers"))?;
        assert_eq!(headers["originator"], "codex_cli_rs");
        Ok(())
    }

    #[test]
    fn login_headers_falls_back_to_default_on_invalid_header_value() -> TestResult {
        // Steuerzeichen sind kein gültiger HeaderValue-Inhalt; `ProviderToml::validate`
        // verhindert das beim Konfigurations-Laden, `login_headers` bleibt trotzdem
        // fail-safe für Aufrufer ohne vorherige Validierung.
        let headers = login_headers(&json!({"tokens":{"access_token":"t"}}), "bad\nvalue")
            .map_err(ctx("login_headers"))?;
        assert_eq!(headers["originator"], DEFAULT_ORIGINATOR);
        Ok(())
    }

    #[test]
    fn from_provider_defaults_originator_to_harw_when_unset() -> TestResult {
        let route = CodexRoute::from_provider(&provider(BASE_URL, ACCESS_POINTER)?)
            .map_err(ctx("from_provider"))?
            .ok_or(TestError::Missing("codex route"))?;
        assert_eq!(route.originator(), DEFAULT_ORIGINATOR);
        Ok(())
    }

    #[test]
    fn from_provider_uses_configured_originator_when_set() -> TestResult {
        let mut config = provider(BASE_URL, ACCESS_POINTER)?;
        config.originator = Some("codex_cli_rs".to_owned());
        let route = CodexRoute::from_provider(&config)
            .map_err(ctx("from_provider"))?
            .ok_or(TestError::Missing("codex route"))?;
        assert_eq!(route.originator(), "codex_cli_rs");
        Ok(())
    }

    #[test]
    fn from_provider_defaults_originator_when_blank() -> TestResult {
        let mut config = provider(BASE_URL, ACCESS_POINTER)?;
        config.originator = Some("   ".to_owned());
        let route = CodexRoute::from_provider(&config)
            .map_err(ctx("from_provider"))?
            .ok_or(TestError::Missing("codex route"))?;
        assert_eq!(route.originator(), DEFAULT_ORIGINATOR);
        Ok(())
    }

    #[test]
    fn session_id_header_is_a_fresh_uuid_v4_shaped_value_each_call() {
        let first = uuid_v4_like();
        let second = uuid_v4_like();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(first.as_bytes()[14], b'4');
    }

    #[tokio::test]
    async fn access_token_needs_refresh_true_for_soon_expiring_jwt() -> TestResult {
        let home = std::env::temp_dir().join(format!(
            "harw-provider-http-codex-refresh-check-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".codex")).map_err(ctx("create .codex dir"))?;
        let auth_path = home.join(".codex/auth.json");

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system time"))?
            .as_secs() as i64;
        let header = base64_url_no_pad(b"{}");
        let payload = base64_url_no_pad(format!("{{\"exp\":{}}}", now + 30).as_bytes());
        let soon_expiring = format!("{header}.{payload}.sig");
        std::fs::write(
            &auth_path,
            json!({"tokens":{"access_token": soon_expiring, "refresh_token": "r"}}).to_string(),
        )
        .map_err(ctx("write auth.json"))?;

        let route = CodexRoute {
            path: auth_path.to_string_lossy().into_owned(),
            originator: DEFAULT_ORIGINATOR.to_owned(),
        };
        // Ohne HOME-Bindung an die Allowlist liest `read_external_cli_credential`
        // nichts; direkt aus der Datei prüfen genügt für diesen Unit-Test.
        let raw = std::fs::read_to_string(&auth_path).map_err(ctx("read auth.json"))?;
        let document: Value = serde_json::from_str(&raw).map_err(ctx("parse auth.json"))?;
        let token = document
            .pointer(ACCESS_POINTER)
            .and_then(Value::as_str)
            .ok_or(TestError::Missing("access_token pointer"))?;
        assert!(harw_oauth::jwt_needs_refresh(
            token,
            PROACTIVE_REFRESH_WINDOW_SECONDS
        ));
        let _ = route.validate_login();

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    fn base64_url_no_pad(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn payload_uses_backend_contract() {
        let mut body = json!({"stream":false,"store":true,"max_output_tokens":500});
        prepare_body(&mut body);
        assert_eq!(body, json!({"stream":true,"store":false,"instructions":""}));
    }

    #[test]
    fn fragmented_sse_preserves_tools_usage_and_reasoning() -> TestResult {
        let response = json!({"status":"completed","output":[
            {"type":"reasoning","encrypted_content":"opaque"},
            {"type":"function_call","call_id":"call-1","name":"read","arguments":"{}"}
        ],"usage":{"input_tokens":10,"output_tokens":2}});
        let stream = format!(
            "event: response.created\r\ndata: {{\"type\":\"response.created\"}}\r\n\r\ndata: {}\r\n\r\n",
            json!({"type":"response.completed","response":response})
        );
        let mut decoder = ResponseDecoder::default();
        let mut result = None;
        for chunk in stream.as_bytes().chunks(3) {
            result = decoder
                .push(chunk, None)
                .map_err(ctx("decoder push"))?
                .or(result);
        }
        assert_eq!(result, Some(response));
        Ok(())
    }

    #[test]
    fn sse_failure_and_incomplete_are_not_successful_tool_responses() -> TestResult {
        let Err(error) = ResponseDecoder::default().push(
            b"data: {\"type\":\"error\",\"message\":\"private\"}\n\n",
            None,
        ) else {
            return Err(TestError::Unexpected(
                "decoder push must fail on an error event".into(),
            ));
        };
        assert!(error.to_string().find("private").is_none());
        let value = ResponseDecoder::default()
            .push(
                b"data: {\"type\":\"response.incomplete\",\"response\":{\"output\":[]}}\n\n",
                None,
            )
            .map_err(ctx("decoder push"))?
            .ok_or(TestError::Missing("decoded response value"))?;
        assert_eq!(value["status"], "incomplete");
        assert!(
            ResponseDecoder::default()
                .push(b"data: not-json\n\n", None)
                .is_err()
        );
        assert!(
            ResponseDecoder::default()
                .push(b"data: {\"type\":\"response.completed\"}\n\n", None)
                .is_err()
        );
        Ok(())
    }
}
