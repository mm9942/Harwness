//! Der Webdienst `harw admin serve`.
//!
//! # Sicherheitsmodell
//! Der Dienst bindet nur Loopback und ist dafür gedacht, über einen Tunnel
//! veröffentlicht zu werden; er muss auch dann sicher sein, wenn jeder ihn
//! anspricht:
//! - Ohne Anmeldung liefert nur die Anmeldeseite und die zwei Enrollment-
//!   Endpunkte etwas; alles andere ist ein generisches 404 / Redirect.
//! - Die Enrollment-Endpunkte geben nur ein einmaliges Zufalls-Nonce heraus und
//!   nehmen Anfragen mit gültigem ML-DSA-Besitznachweis an; freigeben kann nur
//!   ein angemeldeter Admin.
//! - Anmeldung: Argon2id, konstant-zeitige Vergleiche, immer gleicher
//!   Rechenaufwand (Dummy-Hash), generische Fehler, Ratenbegrenzung und
//!   exponentielle Sperre je IP und je Konto, optional TOTP mit Replay-Schutz.
//! - Sitzung: zufälliges Token (nur dessen BLAKE3-Hash liegt im Speicher),
//!   `__Host-`-Cookie (Secure, HttpOnly, SameSite=Strict), kurze Idle- und
//!   Gesamtlebensdauer, CSRF-Token an allen POSTs, Origin- und Host-Prüfung.
//! - Strenge Header: CSP mit Nonce, `no-store`, `nosniff`, `DENY`, kein Referrer.
//! - Es gibt keine Dateiauslieferung und kein Verzeichnislisting.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use harw_config::{AdminSection, ResolvedConfig};
use harw_infra_client::KeyRef;
use harw_types::PermissionTier;
use http::{HeaderMap, Method, Response, StatusCode, header};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use subtle::ConstantTimeEq as _;
use tokio::net::TcpListener;
use zeroize::Zeroizing;

use super::account::{self, Account, MAX_PASSWORD_BYTES, now_unix};
use super::audit;
use super::enroll::{self, EnrollState, Pending, ServerIdentity};
use super::{host_of, public_base, totp};

const SESSION_COOKIE: &str = "__Host-hs";
const LOGIN_COOKIE: &str = "__Host-hl";
const SESSION_IDLE: Duration = Duration::from_secs(600);
const SESSION_MAX: Duration = Duration::from_secs(1800);
const LOGIN_TOKEN_TTL: Duration = Duration::from_secs(600);
const MAX_SESSIONS: usize = 8;
const MAX_LOGIN_TOKENS: usize = 2000;
const MAX_CONNECTIONS: usize = 128;
const MAX_BODY: usize = 32 * 1024;
const NODE_ID_KEY_NAMESPACE: &str = "harw.node-identity";
const GENERIC_FAIL: &str = "Anmeldung fehlgeschlagen.";

struct Session {
    csrf: String,
    created: Instant,
    last_seen: Instant,
}

/// Ratenbegrenzer und IP-Sperre (nur im Speicher).
#[derive(Default)]
struct Limiter {
    hits: HashMap<(&'static str, String), (Instant, u32)>,
    fails: HashMap<String, (u32, Option<Instant>, Instant)>,
}

const MAX_TRACKED: usize = 10_000;

impl Limiter {
    /// Zählt einen Zugriff; `false`, wenn das Limit im Fenster überschritten ist.
    fn hit(&mut self, kind: &'static str, ip: &str, limit: u32, window: Duration) -> bool {
        if self.hits.len() > MAX_TRACKED {
            self.hits.retain(|_, (start, _)| start.elapsed() < window);
        }
        // Bei Überlauf teilen sich neue Quellen einen Eimer (fail closed).
        let key_ip = if self.hits.len() > MAX_TRACKED { "*" } else { ip };
        let entry = self
            .hits
            .entry((kind, key_ip.to_owned()))
            .or_insert((Instant::now(), 0));
        if entry.0.elapsed() >= window {
            *entry = (Instant::now(), 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1 <= limit
    }

    fn ip_blocked(&self, ip: &str) -> bool {
        self.fails
            .get(ip)
            .and_then(|(_, until, _)| *until)
            .is_some_and(|until| until > Instant::now())
    }

    /// Fehlversuch: ab 5 exponentielle Sperre (30 s · 2^(n-5), max. 1 h).
    fn ip_failure(&mut self, ip: &str) {
        if self.fails.len() > MAX_TRACKED {
            self.fails
                .retain(|_, (_, _, seen)| seen.elapsed() < Duration::from_secs(3600));
        }
        let entry = self
            .fails
            .entry(ip.to_owned())
            .or_insert((0, None, Instant::now()));
        entry.0 = entry.0.saturating_add(1);
        entry.2 = Instant::now();
        if entry.0 >= 5 {
            let wait = (30_u64 << (entry.0 - 5).min(7)).min(3600);
            entry.1 = Some(Instant::now() + Duration::from_secs(wait));
        }
    }

    fn ip_success(&mut self, ip: &str) {
        self.fails.remove(ip);
    }
}

struct App {
    home: PathBuf,
    remote_dir: PathBuf,
    admin: AdminSection,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
    account: Mutex<Account>,
    totp_secret: Option<Zeroizing<Vec<u8>>>,
    server: ServerIdentity,
    gateway_addr: Option<SocketAddr>,
    sessions: Mutex<HashMap<String, Session>>,
    login_tokens: Mutex<HashMap<String, Instant>>,
    enroll: Mutex<EnrollState>,
    limiter: Mutex<Limiter>,
    argon_gate: tokio::sync::Semaphore,
    restart_lock: tokio::sync::Mutex<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        // Ohne Zufall gibt es kein Token: Aufrufer behandeln leeren Text als Fehler.
        return String::new();
    }
    data_encoding::BASE64URL_NOPAD.encode(&bytes)
}

fn digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn ct_eq(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() => out.push('?'),
            c => out.push(c),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_form(raw: &str) -> HashMap<String, String> {
    raw.split('&')
        .take(16)
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (!k.is_empty()).then(|| (percent_decode(k), percent_decode(v)))
        })
        .collect()
}

fn valid_req(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|part| {
            let (k, v) = part.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
}

/// Antwort mit Sicherheitsheadern.
struct Reply {
    status: StatusCode,
    content_type: &'static str,
    body: String,
    csp: String,
    headers: Vec<(&'static str, String)>,
}

const CSP_NONE: &str = "default-src 'none'; frame-ancestors 'none'; base-uri 'none'";

impl Reply {
    fn plain(status: StatusCode, text: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: text.to_owned(),
            csp: CSP_NONE.to_owned(),
            headers: Vec::new(),
        }
    }

    fn json(status: StatusCode, value: &serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: value.to_string(),
            csp: CSP_NONE.to_owned(),
            headers: Vec::new(),
        }
    }

    fn not_found() -> Self {
        Self::plain(StatusCode::NOT_FOUND, "Nicht gefunden.")
    }

    fn redirect(to: &str) -> Self {
        let mut reply = Self::plain(StatusCode::SEE_OTHER, "");
        reply.headers.push(("location", to.to_owned()));
        reply
    }

    fn html(status: StatusCode, title: &str, body: &str, refresh: Option<&str>) -> Self {
        let nonce = random_token();
        let meta = refresh
            .map(|url| format!("<meta http-equiv=\"refresh\" content=\"0;url={}\">", esc(url)))
            .unwrap_or_default();
        let html = format!(
            "<!doctype html><html lang=\"de\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex,nofollow\">{meta}<title>{}</title>\
<style nonce=\"{nonce}\">{STYLE}</style></head><body><main>{body}</main></body></html>",
            esc(title)
        );
        Self {
            status,
            content_type: "text/html; charset=utf-8",
            body: html,
            csp: format!(
                "default-src 'none'; style-src 'nonce-{nonce}'; form-action 'self'; \
frame-ancestors 'none'; base-uri 'none'"
            ),
            headers: Vec::new(),
        }
    }

    fn with_cookie(mut self, cookie: String) -> Self {
        self.headers.push(("set-cookie", cookie));
        self
    }

    fn into_response(self) -> Response<Full<Bytes>> {
        let mut builder = Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, self.content_type)
            .header(header::CACHE_CONTROL, "no-store, max-age=0")
            .header(header::PRAGMA, "no-cache")
            .header("content-security-policy", self.csp)
            .header("x-content-type-options", "nosniff")
            .header("x-frame-options", "DENY")
            .header("referrer-policy", "no-referrer")
            .header("cross-origin-opener-policy", "same-origin")
            .header("cross-origin-resource-policy", "same-origin")
            .header("permissions-policy", "camera=(), microphone=(), geolocation=()")
            .header("strict-transport-security", "max-age=31536000");
        for (name, value) in self.headers {
            builder = builder.header(name, value);
        }
        builder
            .body(Full::new(Bytes::from(self.body)))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
    }
}

const STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;margin:0;background:#101418;color:#e8edf2}\
main{max-width:46rem;margin:0 auto;padding:1rem}h1{font-size:1.5rem}h2{font-size:1.15rem;margin-top:2rem}\
table{width:100%;border-collapse:collapse;font-size:.9rem}td,th{text-align:left;padding:.4rem;border-bottom:1px solid #2a323b;word-break:break-all}\
input,select,button{font:inherit;padding:.6rem;margin:.2rem 0;border-radius:.4rem;border:1px solid #3a4652;background:#1a2027;color:inherit;width:100%;box-sizing:border-box}\
button,a.btn{background:#2f7dd1;border:0;color:#fff;cursor:pointer;display:inline-block;padding:.7rem 1rem;border-radius:.4rem;text-decoration:none;width:auto}\
button.warn{background:#b3402f}.err{color:#ff9d8f}.ok{color:#8fe0a0}.muted{color:#8b98a6;font-size:.85rem}\
form.inline{display:inline}label{display:block;margin-top:.6rem}input[type=checkbox]{width:auto;margin-right:.5rem}";

fn fmt_age(secs: u64) -> String {
    format!("{}:{:02} min", secs / 60, secs % 60)
}

impl App {
    fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> String {
        // Hinter einem lokalen Tunnel-Daemon steht die echte Adresse im Header.
        if peer.ip().is_loopback()
            && let Some(ip) = headers
                .get("cf-connecting-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<IpAddr>().ok())
        {
            return ip.to_string();
        }
        peer.ip().to_string()
    }

    fn host_allowed(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|host| self.allowed_hosts.iter().any(|h| h == &host.to_ascii_lowercase()))
    }

    fn origin_allowed(&self, headers: &HeaderMap) -> bool {
        match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            None => true,
            Some(origin) => self.allowed_origins.iter().any(|o| o == &origin.to_ascii_lowercase()),
        }
    }

    /// Angemeldete Sitzung: liefert (Token-Hash, CSRF-Token).
    fn session(&self, headers: &HeaderMap) -> Option<(String, String)> {
        let token = cookie_value(headers, SESSION_COOKIE)?;
        let key = digest(&token);
        let mut sessions = lock(&self.sessions);
        sessions.retain(|_, s| s.last_seen.elapsed() < SESSION_IDLE && s.created.elapsed() < SESSION_MAX);
        let session = sessions.get_mut(&key)?;
        session.last_seen = Instant::now();
        Some((key, session.csrf.clone()))
    }

    fn csrf_ok(session_csrf: &str, form: &HashMap<String, String>) -> bool {
        form.get("csrf").is_some_and(|given| ct_eq(given, session_csrf))
    }

    fn audit(&self, event: &str, ip: &str, detail: serde_json::Value) {
        audit::record(&self.home, event, ip, detail);
    }

    fn session_cookie(value: &str, max_age: u64) -> String {
        format!("{SESSION_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}")
    }

    fn login_cookie(value: &str, max_age: u64) -> String {
        format!("{LOGIN_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}")
    }

    // ── Seiten ──────────────────────────────────────────────────────────

    fn login_page(&self, status: StatusCode, message: Option<&str>, req: Option<&str>) -> Reply {
        let token = random_token();
        if token.is_empty() {
            return Reply::plain(StatusCode::INTERNAL_SERVER_ERROR, "Fehler.");
        }
        {
            let mut tokens = lock(&self.login_tokens);
            tokens.retain(|_, issued| issued.elapsed() < LOGIN_TOKEN_TTL);
            if tokens.len() >= MAX_LOGIN_TOKENS {
                return Reply::plain(StatusCode::SERVICE_UNAVAILABLE, "Bitte später erneut versuchen.");
            }
            tokens.insert(digest(&token), Instant::now());
        }
        let totp_field = if self.totp_secret.is_some() {
            "<label>Code der Authenticator-App<input name=\"totp\" inputmode=\"numeric\" autocomplete=\"one-time-code\" maxlength=\"6\"></label>"
        } else {
            ""
        };
        let req_field = req
            .filter(|r| valid_req(r))
            .map(|r| format!("<input type=\"hidden\" name=\"req\" value=\"{}\">", esc(r)))
            .unwrap_or_default();
        let message = message
            .map(|m| format!("<p class=\"err\">{}</p>", esc(m)))
            .unwrap_or_default();
        let body = format!(
            "<h1>Harw Admin</h1>{message}<form method=\"post\" action=\"/login\" autocomplete=\"off\">\
<input type=\"hidden\" name=\"csrf\" value=\"{token}\">{req_field}\
<label>Benutzername<input name=\"username\" autocomplete=\"username\" maxlength=\"64\" required></label>\
<label>Passwort<input name=\"password\" type=\"password\" autocomplete=\"current-password\" maxlength=\"{MAX_PASSWORD_BYTES}\" required></label>\
{totp_field}<button type=\"submit\">Anmelden</button></form>"
        );
        Reply::html(status, "Anmelden", &body, None).with_cookie(Self::login_cookie(&token, 600))
    }

    fn message_page(&self, status: StatusCode, title: &str, text: &str, csrf: &str) -> Reply {
        let body = format!(
            "<h1>{}</h1><p>{}</p><p><a class=\"btn\" href=\"/\">Zurück</a></p>{}",
            esc(title),
            esc(text),
            logout_form(csrf)
        );
        Reply::html(status, title, &body, None)
    }

    fn dashboard(&self, csrf: &str) -> Reply {
        let pending = lock(&self.enroll).list();
        let mut body = String::from("<h1>Gerät hinzufügen</h1>");
        body.push_str("<p>Öffne in der Harw-App auf dem Handy <b>„Mit Admin-Konto einrichten“</b>. Die Anfrage erscheint hier; vergleiche den Prüfcode mit dem in der App.</p>");
        body.push_str("<h2>Ausstehende Anfragen</h2>");
        if pending.is_empty() {
            body.push_str("<p class=\"muted\">Keine offenen Anfragen.</p>");
        } else {
            body.push_str("<table><tr><th>Gerät</th><th>Prüfcode</th><th>Von</th><th>Läuft ab</th><th></th></tr>");
            for p in &pending {
                body.push_str(&format!(
                    "<tr><td>{}<br><span class=\"muted\">{}</span></td><td>{}</td><td>{}</td><td>{}</td>\
<td><a class=\"btn\" href=\"/enroll?req={}\">Prüfen</a></td></tr>",
                    esc(&p.label),
                    esc(&p.node_id),
                    esc(&p.fingerprint()),
                    esc(&p.ip),
                    fmt_age(p.remaining_secs()),
                    esc(&p.id)
                ));
            }
            body.push_str("</table>");
        }
        body.push_str("<h2>Geräte</h2>");
        match crate::device_cmd::list(&self.remote_dir) {
            Err(_) => body.push_str("<p class=\"err\">Die Geräte-Registry ist nicht lesbar.</p>"),
            Ok(report) => {
                let devices = report.json()["devices"].as_array().cloned().unwrap_or_default();
                if devices.is_empty() {
                    body.push_str("<p class=\"muted\">Noch kein Gerät enrollt.</p>");
                } else {
                    body.push_str("<table><tr><th>Gerät</th><th>Tier</th><th>Status</th><th>Prüfcode</th><th></th></tr>");
                    for d in &devices {
                        let device = d["device"].as_str().unwrap_or("");
                        let active = d["status"].as_str() == Some("active");
                        let action = if active {
                            format!(
                                "<form class=\"inline\" method=\"post\" action=\"/revoke\">\
<input type=\"hidden\" name=\"csrf\" value=\"{}\"><input type=\"hidden\" name=\"device\" value=\"{}\">\
<button class=\"warn\" type=\"submit\">Widerrufen</button></form>",
                                esc(csrf),
                                esc(device)
                            )
                        } else {
                            String::new()
                        };
                        body.push_str(&format!(
                            "<tr><td>{}<br><span class=\"muted\">{}</span></td><td>{}</td><td>{}</td><td>{}</td><td>{action}</td></tr>",
                            esc(d["label"].as_str().unwrap_or("")),
                            esc(device),
                            esc(d["tier"].as_str().unwrap_or("")),
                            esc(d["status"].as_str().unwrap_or("")),
                            esc(d["fingerprint"].as_str().unwrap_or("-")),
                        ));
                    }
                    body.push_str("</table>");
                }
            }
        }
        body.push_str("<h2>Protokoll</h2><table>");
        for (ts, event, ip, detail) in audit::tail(&self.home, 15) {
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"muted\">{}</td></tr>",
                ts,
                esc(&event),
                esc(&ip),
                esc(&detail)
            ));
        }
        body.push_str("</table>");
        if !self.admin.restart_gateway {
            body.push_str(&format!(
                "<p class=\"muted\">Nach einer Freigabe muss das Gateway einmal neu gestartet werden: <code>systemctl --user restart {}</code></p>",
                esc(&self.admin.gateway_unit)
            ));
        }
        body.push_str(&logout_form(csrf));
        Reply::html(StatusCode::OK, "Gerät hinzufügen", &body, None)
    }

    fn enroll_page(&self, p: &Pending, csrf: &str) -> Reply {
        let body = format!(
            "<h1>Gerät prüfen</h1><table>\
<tr><th>Name</th><td>{label}</td></tr><tr><th>Node-ID</th><td>{node}</td></tr>\
<tr><th>Prüfcode</th><td><b>{fp}</b></td></tr><tr><th>Angefragt von</th><td>{ip}</td></tr>\
<tr><th>Läuft ab in</th><td>{left}</td></tr></table>\
<p>Gib das Gerät nur frei, wenn der Prüfcode mit dem in der Harw-App übereinstimmt.</p>\
<form method=\"post\" action=\"/approve\"><input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
<input type=\"hidden\" name=\"req\" value=\"{id}\">\
<label>Berechtigungsstufe<select name=\"tier\">\
<option value=\"observer\">observer (nur lesen)</option><option value=\"operator\">operator</option>\
<option value=\"maintainer\" selected>maintainer</option></select></label>\
<label><input type=\"checkbox\" name=\"approve\" value=\"on\" checked>Darf Freigaben (Approvals) erteilen</label>\
<button type=\"submit\">Freigeben</button></form>\
<form method=\"post\" action=\"/reject\"><input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
<input type=\"hidden\" name=\"req\" value=\"{id}\"><button class=\"warn\" type=\"submit\">Ablehnen</button></form>\
<p><a href=\"/\">Zurück</a></p>",
            label = esc(&p.label),
            node = esc(&p.node_id),
            fp = esc(&p.fingerprint()),
            ip = esc(&p.ip),
            left = fmt_age(p.remaining_secs()),
            csrf = esc(csrf),
            id = esc(&p.id),
        );
        Reply::html(StatusCode::OK, "Gerät prüfen", &body, None)
    }

    // ── Handler ─────────────────────────────────────────────────────────

    async fn route(self: &Arc<Self>, ip: &str, req: http::Request<Incoming>) -> Reply {
        let (parts, body) = req.into_parts();
        let path = parts.uri.path().to_owned();
        let query = parse_form(parts.uri.query().unwrap_or(""));
        let method = parts.method.clone();
        if path.len() > 200 {
            return Reply::not_found();
        }
        let is_api = path.starts_with("/api/");
        let form = if method == Method::POST {
            let ctype = parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let expected = if is_api { "application/json" } else { "application/x-www-form-urlencoded" };
            if !ctype.starts_with(expected) {
                return Reply::plain(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Nicht unterstützt.");
            }
            if !is_api && !self.origin_allowed(&parts.headers) {
                return Reply::plain(StatusCode::FORBIDDEN, "Nicht erlaubt.");
            }
            match Limited::new(body, MAX_BODY).collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => return Reply::plain(StatusCode::PAYLOAD_TOO_LARGE, "Zu groß."),
            }
        } else {
            Bytes::new()
        };
        match (method.as_str(), path.as_str()) {
            ("GET", "/api/enroll/nonce") => self.api_nonce(ip),
            ("POST", "/api/enroll") => self.api_enroll(ip, &form),
            ("GET", "/login") => self.login_page(StatusCode::OK, None, query.get("req").map(String::as_str)),
            ("POST", "/login") => {
                let form = parse_form(&String::from_utf8_lossy(&form));
                self.login(ip, &parts.headers, &form).await
            }
            ("GET", "/") | ("GET", "/enroll") | ("POST", "/approve" | "/reject" | "/revoke" | "/logout") => {
                let Some((key, csrf)) = self.session(&parts.headers) else {
                    return if method == Method::GET {
                        match query.get("req").filter(|r| valid_req(r)) {
                            Some(r) if path == "/enroll" => Reply::redirect(&format!("/login?req={r}")),
                            _ => Reply::redirect("/login"),
                        }
                    } else {
                        Reply::redirect("/login")
                    };
                };
                if method == Method::GET {
                    return self.get_authed(&path, &query, &csrf);
                }
                let form = parse_form(&String::from_utf8_lossy(&form));
                if !Self::csrf_ok(&csrf, &form) {
                    self.audit("csrf_rejected", ip, serde_json::json!({ "path": path }));
                    return Reply::plain(StatusCode::FORBIDDEN, "Nicht erlaubt.");
                }
                self.post_authed(ip, &path, &form, &key, &csrf).await
            }
            _ => Reply::not_found(),
        }
    }

    fn get_authed(&self, path: &str, query: &HashMap<String, String>, csrf: &str) -> Reply {
        if path == "/enroll" {
            let found = query
                .get("req")
                .filter(|r| valid_req(r))
                .and_then(|r| lock(&self.enroll).get(r));
            return match found {
                Some(p) => self.enroll_page(&p, csrf),
                None => self.message_page(
                    StatusCode::NOT_FOUND,
                    "Anfrage nicht gefunden",
                    "Die Anfrage ist unbekannt oder abgelaufen (nach 10 Minuten). Bitte in der App neu starten.",
                    csrf,
                ),
            };
        }
        self.dashboard(csrf)
    }

    async fn post_authed(
        self: &Arc<Self>,
        ip: &str,
        path: &str,
        form: &HashMap<String, String>,
        session_key: &str,
        csrf: &str,
    ) -> Reply {
        match path {
            "/logout" => {
                lock(&self.sessions).remove(session_key);
                self.audit("logout", ip, serde_json::json!({}));
                Reply::redirect("/login").with_cookie(Self::session_cookie("", 0))
            }
            "/reject" => {
                let id = form.get("req").map(String::as_str).unwrap_or("");
                if let Some(p) = valid_req(id).then(|| lock(&self.enroll).take(id)).flatten() {
                    self.audit(
                        "enroll_rejected",
                        ip,
                        serde_json::json!({ "device": p.node_id, "fingerprint": p.fingerprint() }),
                    );
                }
                Reply::redirect("/")
            }
            "/revoke" => {
                let device = form.get("device").map(String::as_str).unwrap_or("");
                let remote_dir = self.remote_dir.clone();
                let device_owned = device.to_owned();
                let result = crate::device_cmd::revoke(&remote_dir, &device_owned);
                match result {
                    Ok(outcome) => {
                        self.audit(
                            "device_revoked",
                            ip,
                            serde_json::json!({ "device": device_owned, "changed": outcome.json()["changed"] }),
                        );
                        Reply::redirect("/")
                    }
                    Err(_) => {
                        self.audit("device_revoke_failed", ip, serde_json::json!({ "device": device_owned }));
                        self.message_page(
                            StatusCode::BAD_REQUEST,
                            "Widerruf fehlgeschlagen",
                            "Das Gerät ist unbekannt oder die Registry nicht schreibbar.",
                            csrf,
                        )
                    }
                }
            }
            _ => self.approve(ip, form, csrf).await,
        }
    }

    async fn approve(self: &Arc<Self>, ip: &str, form: &HashMap<String, String>, csrf: &str) -> Reply {
        let id = form.get("req").map(String::as_str).unwrap_or("");
        let tier = match form.get("tier").map(String::as_str) {
            Some("observer") => PermissionTier::Observer,
            Some("operator") => PermissionTier::Operator,
            Some("maintainer") => PermissionTier::Maintainer,
            _ => return Reply::plain(StatusCode::BAD_REQUEST, "Ungültig."),
        };
        let approve_optin = form.get("approve").is_some_and(|v| v == "on");
        let Some(pending) = valid_req(id).then(|| lock(&self.enroll).take(id)).flatten() else {
            return self.message_page(
                StatusCode::NOT_FOUND,
                "Anfrage nicht gefunden",
                "Die Anfrage ist unbekannt, abgelaufen oder bereits bearbeitet.",
                csrf,
            );
        };
        if let Err(error) = enroll::apply_approval(
            &self.remote_dir,
            &self.admin.tenant,
            &pending,
            tier,
            approve_optin,
        ) {
            self.audit(
                "enroll_failed",
                ip,
                serde_json::json!({ "device": pending.node_id, "reason": error }),
            );
            return self.message_page(StatusCode::CONFLICT, "Freigabe fehlgeschlagen", &error, csrf);
        }
        self.audit(
            "enroll_approved",
            ip,
            serde_json::json!({
                "device": pending.node_id,
                "label": pending.label,
                "tier": crate::device_cmd::tier_name(tier),
                "approve": approve_optin,
                "fingerprint": pending.fingerprint(),
                "requested_from": pending.ip,
            }),
        );
        let restart_note = if self.admin.restart_gateway {
            match self.restart_gateway(ip).await {
                Ok(()) => "Das Gateway wurde neu gestartet, damit der Schlüssel des Geräts gilt.".to_owned(),
                Err(error) => format!(
                    "Das Gateway konnte nicht automatisch neu gestartet werden ({error}). Bitte manuell: systemctl --user restart {}",
                    self.admin.gateway_unit
                ),
            }
        } else {
            format!(
                "Das Gateway muss einmal neu gestartet werden, damit der Schlüssel gilt: systemctl --user restart {}",
                self.admin.gateway_unit
            )
        };
        let link = self.server.deep_link(&pending.id);
        let body = format!(
            "<h1>Gerät freigegeben</h1><p class=\"ok\">{} ist eingetragen.</p><p>{}</p>\
<p><a class=\"btn\" href=\"{}\">Harw-App öffnen</a></p>\
<p class=\"muted\">Öffnet sich die App nicht automatisch, tippe auf den Knopf.</p><p><a href=\"/\">Zur Übersicht</a></p>",
            esc(&pending.label),
            esc(&restart_note),
            esc(&link)
        );
        Reply::html(StatusCode::OK, "Gerät freigegeben", &body, Some(&link))
    }

    /// `systemctl --user restart <unit>` (feste Argumente, keine Shell), danach
    /// bis zu 30 s warten, bis das Gateway wieder lauscht.
    async fn restart_gateway(&self, ip: &str) -> Result<(), String> {
        let _guard = self.restart_lock.lock().await;
        let unit = self.admin.gateway_unit.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut child = std::process::Command::new("systemctl")
                .args(["--user", "restart", &unit])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => return Ok(()),
                    Ok(Some(status)) => return Err(format!("systemctl: {status}")),
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
                    Ok(None) => {
                        let _ = child.kill();
                        return Err("Zeitüberschreitung".to_owned());
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        })
        .await
        .map_err(|_| "interner Fehler".to_owned())
        .and_then(|r| r);
        self.audit(
            "gateway_restart",
            ip,
            serde_json::json!({ "unit": self.admin.gateway_unit, "ok": outcome.is_ok() }),
        );
        outcome?;
        if let Some(addr) = self.gateway_addr {
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if tokio::net::TcpStream::connect(addr).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        Ok(())
    }

    fn api_nonce(&self, ip: &str) -> Reply {
        if !lock(&self.limiter).hit("nonce", ip, 20, Duration::from_secs(600)) {
            return Reply::json(StatusCode::TOO_MANY_REQUESTS, &serde_json::json!({ "error": "zu viele Anfragen" }));
        }
        match lock(&self.enroll).issue_nonce() {
            Ok(nonce) => Reply::json(StatusCode::OK, &serde_json::json!({ "nonce": nonce, "expires_in": 120 })),
            Err(_) => Reply::json(StatusCode::SERVICE_UNAVAILABLE, &serde_json::json!({ "error": "nicht verfügbar" })),
        }
    }

    fn api_enroll(&self, ip: &str, body: &[u8]) -> Reply {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Body {
            nonce: String,
            node_id: String,
            public_key_hex: String,
            label: String,
            signature_hex: String,
        }
        let denied = || Reply::json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": "abgelehnt" }));
        if !lock(&self.limiter).hit("enroll", ip, 10, Duration::from_secs(600)) {
            return Reply::json(StatusCode::TOO_MANY_REQUESTS, &serde_json::json!({ "error": "zu viele Anfragen" }));
        }
        let Ok(parsed) = serde_json::from_slice::<Body>(body) else {
            return denied();
        };
        let result = lock(&self.enroll).submit(
            &parsed.nonce,
            &parsed.node_id,
            &parsed.public_key_hex,
            &parsed.label,
            &parsed.signature_hex,
            ip,
        );
        match result {
            Ok(id) => {
                let fingerprint = lock(&self.enroll).get(&id).map(|p| p.fingerprint()).unwrap_or_default();
                self.audit(
                    "enroll_requested",
                    ip,
                    serde_json::json!({ "device": parsed.node_id, "fingerprint": fingerprint }),
                );
                Reply::json(
                    StatusCode::OK,
                    &serde_json::json!({
                        "req": id,
                        "fingerprint": fingerprint,
                        "expires_in": enroll::REQUEST_TTL.as_secs(),
                    }),
                )
            }
            Err(_) => {
                self.audit("enroll_refused", ip, serde_json::json!({}));
                denied()
            }
        }
    }

    async fn login(&self, ip: &str, headers: &HeaderMap, form: &HashMap<String, String>) -> Reply {
        let req = form.get("req").map(String::as_str);
        let retry = || self.login_page(StatusCode::TOO_MANY_REQUESTS, Some("Zu viele Versuche. Bitte später erneut versuchen."), req);
        {
            let mut limiter = lock(&self.limiter);
            if !limiter.hit("login", ip, 10, Duration::from_secs(300)) || limiter.ip_blocked(ip) {
                drop(limiter);
                return retry();
            }
        }
        // Doppeltes Submit-Cookie plus serverseitig ausgestelltes, einmaliges Token.
        let csrf_ok = match (cookie_value(headers, LOGIN_COOKIE), form.get("csrf")) {
            (Some(cookie), Some(field)) if ct_eq(&cookie, field) => {
                lock(&self.login_tokens).remove(&digest(&cookie)).is_some()
            }
            _ => false,
        };
        if !csrf_ok {
            return self.login_page(StatusCode::FORBIDDEN, Some("Die Seite war abgelaufen. Bitte erneut versuchen."), req);
        }
        let username = form.get("username").map(String::as_str).unwrap_or("");
        let password = Zeroizing::new(form.get("password").cloned().unwrap_or_default());
        let code = form.get("totp").map(String::as_str).unwrap_or("");

        let snapshot = lock(&self.account).clone();
        let user_match = ct_eq(username, &snapshot.username);
        let now = now_unix();
        let locked = snapshot.is_locked(now);
        let hash = if user_match { snapshot.password_hash.clone() } else { account::dummy_hash().to_owned() };
        let too_long = password.len() > MAX_PASSWORD_BYTES;
        // Immer derselbe Aufwand, auch bei Sperre und falschem Benutzer.
        let pw_ok = match self.argon_gate.acquire().await {
            Ok(_permit) if !too_long => {
                let pw = password.clone();
                tokio::task::spawn_blocking(move || account::verify_password(&pw, &hash))
                    .await
                    .unwrap_or(false)
            }
            _ => false,
        };
        let mut totp_step = None;
        let mut totp_ok = true;
        if let Some(secret) = &self.totp_secret {
            totp_step = totp::verify(secret, code, now, snapshot.totp_last_step);
            totp_ok = totp_step.is_some();
        }
        let success = user_match && pw_ok && totp_ok && !locked;
        if success {
            let session_token = random_token();
            let csrf = random_token();
            if session_token.is_empty() || csrf.is_empty() {
                return Reply::plain(StatusCode::INTERNAL_SERVER_ERROR, "Fehler.");
            }
            {
                let mut acct = lock(&self.account);
                acct.clear_failures();
                if let Some(step) = totp_step {
                    acct.totp_last_step = step;
                }
                if let Err(error) = acct.save(&self.home) {
                    tracing::warn!(%error, "admin account not persisted");
                }
            }
            {
                let mut sessions = lock(&self.sessions);
                sessions.retain(|_, s| s.last_seen.elapsed() < SESSION_IDLE && s.created.elapsed() < SESSION_MAX);
                if sessions.len() >= MAX_SESSIONS
                    && let Some(oldest) = sessions.iter().min_by_key(|(_, s)| s.created).map(|(k, _)| k.clone())
                {
                    sessions.remove(&oldest);
                }
                sessions.insert(
                    digest(&session_token),
                    Session { csrf, created: Instant::now(), last_seen: Instant::now() },
                );
            }
            lock(&self.limiter).ip_success(ip);
            self.audit("login_ok", ip, serde_json::json!({}));
            let target = match req.filter(|r| valid_req(r)) {
                Some(r) => format!("/enroll?req={r}"),
                None => "/".to_owned(),
            };
            return Reply::redirect(&target)
                .with_cookie(Self::session_cookie(&session_token, SESSION_MAX.as_secs()))
                .with_cookie(Self::login_cookie("", 0));
        }
        lock(&self.limiter).ip_failure(ip);
        if user_match {
            let mut acct = lock(&self.account);
            acct.record_failure(now);
            if let Err(error) = acct.save(&self.home) {
                tracing::warn!(%error, "admin account not persisted");
            }
        }
        self.audit("login_failed", ip, serde_json::json!({}));
        // Dieselbe Antwort für falschen Benutzer, falsches Passwort, falschen
        // Code und Sperre.
        self.login_page(StatusCode::UNAUTHORIZED, Some(GENERIC_FAIL), req)
    }
}

fn logout_form(csrf: &str) -> String {
    format!(
        "<form method=\"post\" action=\"/logout\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">\
<button class=\"warn\" type=\"submit\">Abmelden</button></form>",
        esc(csrf)
    )
}

async fn handle(app: Arc<App>, peer: SocketAddr, req: http::Request<Incoming>) -> Response<Full<Bytes>> {
    let ip = app.client_ip(peer, req.headers());
    if !lock(&app.limiter).hit("req", &ip, 120, Duration::from_secs(60)) {
        return Reply::plain(StatusCode::TOO_MANY_REQUESTS, "Zu viele Anfragen.").into_response();
    }
    if !app.host_allowed(req.headers()) {
        return Reply::not_found().into_response();
    }
    app.route(&ip, req).await.into_response()
}

/// Startet den Webdienst und blockiert bis SIGINT/SIGTERM.
///
/// # Errors
/// Fehlendes Konto, nicht-Loopback-Adresse, fehlende Gateway-Konfiguration,
/// nicht auflösbares TOTP-Geheimnis oder Bind-Fehler.
pub(super) fn serve(home: &std::path::Path, config: &ResolvedConfig) -> Result<(), String> {
    let admin = config.harness.admin.clone();
    let addr: SocketAddr = admin
        .listen
        .parse()
        .map_err(|_| "[admin] listen ist keine gültige Adresse host:port".to_owned())?;
    if !addr.ip().is_loopback() {
        return Err("[admin] listen muss eine Loopback-Adresse sein (127.0.0.1 oder ::1); veröffentliche den Dienst über einen Tunnel.".to_owned());
    }
    if !admin.gateway_unit.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@'))
        || admin.gateway_unit.is_empty()
        || admin.gateway_unit.starts_with('-')
    {
        return Err("[admin] gateway_unit enthält ungültige Zeichen".to_owned());
    }
    let account = Account::load(home)?
        .ok_or_else(|| "Kein Admin-Konto vorhanden: zuerst `harw admin init`.".to_owned())?;
    let base = public_base(&admin)?;

    let listener_cfg = &config.harness.session_listener;
    let node_id = listener_cfg
        .node_id
        .clone()
        .ok_or_else(|| "[session_listener] node_id fehlt (Server-Identität für das Profil)".to_owned())?;
    let infrastructure = config
        .infrastructure
        .as_ref()
        .ok_or_else(|| "[infrastructure] (AuthHub) fehlt: der öffentliche Server-Schlüssel kommt von dort".to_owned())?;
    let endpoint = admin
        .public_endpoint
        .clone()
        .unwrap_or_else(|| listener_cfg.listen.clone());
    let endpoint_addr: SocketAddr = endpoint
        .parse()
        .map_err(|_| "Profil-Endpunkt ist keine gültige Adresse host:port ([admin] public_endpoint)".to_owned())?;
    if endpoint_addr.ip().is_unspecified() || endpoint_addr.port() == 0 {
        return Err("Profil-Endpunkt ist unbrauchbar: [admin] public_endpoint setzen".to_owned());
    }
    // Das TOTP-Geheimnis vor der Runtime auflösen (der Secret-Store ist synchron).
    let totp_secret = match &account.totp_secret_ref {
        None => None,
        Some(reference) => {
            let sealed = crate::secret_store::resolve_sealed(home, config, reference)?;
            let bytes = totp::from_base32(secrecy::ExposeSecret::expose_secret(&sealed))
                .ok_or_else(|| "versiegeltes TOTP-Geheimnis ist unlesbar".to_owned())?;
            Some(Zeroizing::new(bytes))
        }
    };

    let remote_dir = crate::session_serve_remote::remote_state_dir(home)?;
    let state_dir = remote_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| remote_dir.clone());
    let onion_file = listener_cfg
        .onion_dir
        .as_ref()
        .map_or_else(|| state_dir.join(crate::session_listener::ONION_DIR), PathBuf::from)
        .join(crate::session_listener::ONION_LOCATOR_FILE);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| format!("Runtime nicht startbar: {error}"))?;
    runtime.block_on(async move {
        let client = crate::secret_store::authhub_client(infrastructure)?;
        let key = KeyRef::latest(NODE_ID_KEY_NAMESPACE, &node_id).map_err(|e| format!("Schlüsselreferenz: {e}"))?;
        let public_key = client
            .public_key(&key)
            .await
            .map_err(|_| "Der öffentliche Server-Schlüssel ist vom AuthHub nicht lesbar".to_owned())?
            .into_vec();
        let server = ServerIdentity {
            node_id,
            public_key_hex: public_key.iter().map(|b| format!("{b:02x}")).collect(),
            endpoint,
            onion_locator_file: onion_file,
            admin_url: base.clone(),
        };

        let mut allowed_hosts = vec![addr.to_string(), format!("localhost:{}", addr.port())];
        let mut allowed_origins = vec![format!("http://{addr}"), format!("http://localhost:{}", addr.port())];
        if let Some(base) = &base {
            allowed_hosts.push(host_of(base));
            allowed_origins.push(base.to_ascii_lowercase());
        }
        let app = Arc::new(App {
            home: home.to_path_buf(),
            remote_dir,
            admin: admin.clone(),
            allowed_hosts,
            allowed_origins,
            account: Mutex::new(account),
            totp_secret,
            server,
            gateway_addr: listener_cfg.listen.parse().ok(),
            sessions: Mutex::new(HashMap::new()),
            login_tokens: Mutex::new(HashMap::new()),
            enroll: Mutex::new(EnrollState::default()),
            limiter: Mutex::new(Limiter::default()),
            argon_gate: tokio::sync::Semaphore::new(2),
            restart_lock: tokio::sync::Mutex::new(()),
        });
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|error| format!("{addr} nicht bindbar: {error}"))?;
        println!("harw admin lauscht auf http://{addr}/ (nur lokal)");
        match &base {
            Some(base) => println!("Öffentliche Adresse: {base}/ (Tunnel auf http://{addr} zeigen lassen)"),
            None => println!("Hinweis: [admin] public_url fehlt in der globalen config.toml; ohne sie gibt es keine öffentliche Adresse für Handy und Deep-Links."),
        }
        app.audit("serve_started", "local", serde_json::json!({ "listen": addr.to_string() }));
        let gate = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| e.to_string())?;
        loop {
            let (stream, peer) = tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok(pair) => pair,
                    Err(_) => { tokio::time::sleep(Duration::from_millis(50)).await; continue; }
                },
                _ = tokio::signal::ctrl_c() => break,
                _ = sigterm.recv() => break,
            };
            let Ok(permit) = gate.clone().try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let app = app.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req| {
                    let app = app.clone();
                    async move { Ok::<_, Infallible>(handle(app, peer, req).await) }
                });
                let connection = hyper::server::conn::http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(10))
                    .max_buf_size(32 * 1024)
                    .serve_connection(TokioIo::new(stream), service);
                let _ = tokio::time::timeout(Duration::from_secs(60), connection).await;
                drop(permit);
            });
        }
        app_shutdown(&app);
        Ok(())
    })
}

fn app_shutdown(app: &App) {
    lock(&app.sessions).clear();
    app.audit("serve_stopped", "local", serde_json::json!({}));
}
