//! `/dod model` (PL-100 WP 1.9, README §3.2): show, set or clear the DoD
//! triage model `[dod.triage] model` of the bound harw home, plus the
//! provider-locality rule the doctor and the settings views share.
//!
//! # Surfaces
//! - `/dod model [show]` — the effective `[dod.triage]` values (model,
//!   provider, effort, max_tokens, max_parallel), the locality of the triage
//!   provider ([`TriageLocality`]), the configured and the effective
//!   `[dod.watch] mode`, `autoconnect` and `[dod.approve] tui_tty`
//!   ([`TriageSettings`], [`model_text`], [`model_view`]).
//! - `/dod model set <provider>/<model>` — validates the reference
//!   ([`parse_model_ref`]) and writes it into `<home>/config.toml` through
//!   `harw_config::set_dod_triage_model` ([`write_model`]).
//! - `/dod model clear` — removes the key from `<home>/config.toml`.
//!
//! `crate::dod` parses `/dod model …` ([`parse_model_args`]) and calls
//! `model_command`, so this is a subcommand of the existing `dod`
//! operation: operator permission, `channel_reduced` with `model` absent from
//! its channel subcommands (remote channels never reach it) and no model-tool
//! surface. Choosing the model that receives security data is a human
//! decision: no registry profile, no `AUTO_APPROVED_TOOLS` entry and no model
//! tool can set it. The `harw dod model show|set|clear` CLI
//! (`harw-cli/src/dod_cmd.rs`) renders through the same helpers.
//!
//! # Trust boundary
//! `set`/`clear` write only the home layer `<home>/config.toml`; together
//! with the active profile it is the only layer `[dod.triage]` is taken from
//! (threat B15). `/dod model show`, the doctor and `harw dod model show`
//! read the DoD settings from exactly these two layers ([`trusted_config`]),
//! so the provider of the triage model, and with it the locality shown, is
//! never taken from a project's `providers/<name>.toml` (threat B14). A
//! `model` in the active profile takes precedence; the result of
//! `set`/`clear` says so ([`model_change_output`]). There is no silent
//! fallback to the session model: without a triage model, `[dod.watch] mode =
//! "triage"` acts as `notify` (invariant 7,
//! `DodSection::effective_watch_mode`), which every view states
//! ([`NO_TRIAGE_MODEL_NOTE`]); the same holds for a triage provider without
//! a `[providers]` entry or with `enabled = false`
//! ([`TRIAGE_PROVIDER_MISSING_NOTE`], [`TRIAGE_PROVIDER_DISABLED_NOTE`]). The
//! WP 0.3 egress gate (an empty `egress_allow_cidrs` in `/etc/harw-dod`) is
//! checked only by the doctor;
//! the other views say so ([`EGRESS_DOCTOR_HINT`]). Locality is displayed,
//! never enforced (the owner's choice). Errors never echo configuration
//! text, and a rejected model reference is not echoed back either.
//!
//! # Concurrency
//! Synchronous and blocking (small file I/O, like `/settings`). Concurrent
//! writers of the same `config.toml` are serialized by the lock of
//! `harw_config::ConfigWriter`; a second writer gets a busy error and
//! nothing is written.
//!
//! # Errors
//! [`OpError::InvalidArguments`] for a malformed command or model reference,
//! [`OpError::NotAvailable`] without a bound or existing harw home,
//! [`OpError::Execution`] for an unreadable configuration or a failed write
//! (fixed texts, see [`write_model`]).
//!
//! # Examples
//! ```rust
//! use harw_ops::dod_model::{DodModelCommand, TriageLocality, base_url_locality, parse_model_args};
//!
//! assert_eq!(base_url_locality("http://127.0.0.1:11434/v1"), TriageLocality::Local);
//! assert_eq!(base_url_locality("https://openrouter.ai/api/v1"), TriageLocality::Remote);
//! assert_eq!(
//!     parse_model_args(&["set", "openrouter/openai/gpt-4o"]).ok(),
//!     Some(DodModelCommand::Set("openrouter/openai/gpt-4o".to_owned()))
//! );
//! assert!(parse_model_args(&["set", "noslash"]).is_err());
//! ```

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use harw_config::{ConfigError, DodSection, DodWatchMode, ProviderToml, ResolvedConfig};
use harw_operations::{OpContext, OpError, OpOutput};
use serde_json::{Value, json};

use crate::dod::bounded;

/// Usage of `/dod model`.
pub const DOD_MODEL_USAGE: &str = "/dod model [show | set <provider>/<model> | clear]";

/// Note shown wherever no triage model is set while `[dod.watch] mode` asks
/// for triage: the watch acts as `notify` (README §3.2, invariant 7).
pub const NO_TRIAGE_MODEL_NOTE: &str = "kein Triage-Modell gesetzt → notify";

/// Note shown wherever the triage model names a provider without a
/// `[providers]` entry while `[dod.watch] mode` asks for triage: the watch
/// acts as `notify` (no fallback to another provider or the session model).
pub const TRIAGE_PROVIDER_MISSING_NOTE: &str = "Triage-Provider nicht konfiguriert → notify";

/// Note shown wherever the provider of the triage model has `enabled = false`
/// while `[dod.watch] mode` asks for triage: the watch acts as `notify`.
pub const TRIAGE_PROVIDER_DISABLED_NOTE: &str = "Triage-Provider deaktiviert → notify";

/// Label of the same-TTY approval mode `[dod.approve] tui_tty = true`
/// (O12, threat B17; README §3.1 „schwächere Bindung“): reversible
/// interventions only, weaker binding. The one wording of the doctor,
/// `/dod model`, `/settings` and the TUI settings row.
pub const TUI_TTY_NOTE: &str = "schwächere Bindung, nur Freeze/Thaw";

/// The only way to change the triage model, named by read-only views.
pub const TRIAGE_MODEL_HINT: &str = "/dod model set <provider>/<model>";

/// Appended where the configured watch mode resolves to `triage`: the WP 0.3
/// egress gate (an empty `egress_allow_cidrs` in `/etc/harw-dod` turns
/// triage into notify) is checked only by the doctor.
pub const EGRESS_DOCTOR_HINT: &str = "egress allowlist: see /dod doctor";

// Fixed `ConfigError::Invalid` texts of `harw_config` that are safe to show
// (`ConfigWriter` lock and conflict, `set_dod_triage_model` without home).
const WRITER_BUSY: &str = "configuration busy: another writer holds the lock; retry";
const WRITER_CONFLICT: &str = "configuration conflict: reopen the file before saving";
const HOME_MISSING: &str = "harw home does not exist";

// Shown instead of any other configuration error (they may quote config text
// or paths).
const WRITE_FAILED: &str =
    "[dod.triage] model could not be written; the configuration was not changed";
const CONFIG_UNREADABLE: &str = "the harw configuration could not be read (see `harw doctor`)";

/// A parsed `/dod model` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DodModelCommand {
    /// Show the effective triage settings.
    Show,
    /// Write this validated `<provider>/<model>` reference into the home layer.
    Set(String),
    /// Remove `[dod.triage] model` from the home layer.
    Clear,
}

/// Where the provider of the triage model runs, judged by its `base_url`
/// (README §3.2: loopback or a Unix socket is local, everything else remote).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriageLocality {
    /// Exactly `localhost`, `127.0.0.0/8`, `::1` (also IPv4-mapped) or a
    /// Unix socket URL (`unix:`, `http+unix:`, `https+unix:`). A subdomain
    /// such as `foo.localhost` is [`Self::Remote`]: without an RFC 6761
    /// resolver it may be sent to DNS and resolve off-host.
    Local,
    /// Any other host: security data leaves this machine.
    Remote,
    /// The provider is not configured, or its `base_url` names no host.
    Unknown,
}

impl TriageLocality {
    /// Machine value for JSON views: `local`, `remote` or `unknown`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Unknown => "unknown",
        }
    }

    /// Display label for text views: `lokal`, `remote` or `unbekannt`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Local => "lokal",
            Self::Remote => "remote",
            Self::Unknown => "unbekannt",
        }
    }
}

/// Result of a successful [`write_model`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelWriteOutcome {
    /// Written (or nothing to change).
    Saved,
    /// Written, but the directory sync failed: durability across a crash is
    /// uncertain.
    DurabilityUncertain,
}

/// The DoD triage, watch and approval settings of one resolved
/// configuration, ready for display (`/dod model show`, `/settings show`,
/// the doctor).
///
/// # Examples
/// ```rust
/// use std::collections::HashMap;
///
/// use harw_config::DodSection;
/// use harw_ops::dod_model::{NO_TRIAGE_MODEL_NOTE, TriageSettings};
///
/// let settings = TriageSettings::from_config(&DodSection::default(), &HashMap::new());
/// assert_eq!(settings.model, None);
/// assert_eq!(settings.effective_watch_mode.as_str(), "notify");
/// assert_eq!(settings.notes(), vec![NO_TRIAGE_MODEL_NOTE]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriageSettings {
    /// `[dod.triage] model` (`<provider>/<model>`), `None` when unset.
    pub model: Option<String>,
    /// Provider part of [`Self::model`].
    pub provider: Option<String>,
    /// Locality of [`Self::provider`]; `None` without a model.
    pub locality: Option<TriageLocality>,
    /// `enabled` of the provider's `[providers]` entry; `None` without a
    /// model or when the provider is not configured. Catalog-seeded
    /// providers start disabled (`harw provider enable <id>`).
    pub provider_enabled: Option<bool>,
    /// `[dod.triage] effort`; `None`: the model's default.
    pub effort: Option<String>,
    /// `[dod.triage] max_tokens`; `None`: the dispatcher's default.
    pub max_tokens: Option<u32>,
    /// `[dod.triage] max_parallel`.
    pub max_parallel: u8,
    /// `[dod.watch] mode` as configured.
    pub watch_mode: DodWatchMode,
    /// The mode the configuration allows (`triage` without a model is
    /// `notify`, `DodSection::effective_watch_mode`). Config-only: the WP 0.3
    /// egress gate (an empty `egress_allow_cidrs` in `/etc/harw-dod`) is not
    /// part of it; only the doctor reads the system configuration
    /// ([`EGRESS_DOCTOR_HINT`]).
    pub effective_watch_mode: DodWatchMode,
    /// `[dod.watch] autoconnect`.
    pub autoconnect: bool,
    /// `[dod.approve] tui_tty`.
    pub tui_tty: bool,
}

impl TriageSettings {
    /// Collects the settings from a resolved `[dod]` section and the
    /// configured providers.
    ///
    /// # Arguments
    /// - `dod` (`&DodSection`): the resolved `[dod]` section.
    /// - `providers` (`&HashMap<String, ProviderToml>`): the configured
    ///   providers by name (`ResolvedConfig::providers`).
    ///
    /// # Returns
    /// The display settings; [`Self::locality`] is `None` without a model and
    /// [`TriageLocality::Unknown`] for an unconfigured provider,
    /// [`Self::provider_enabled`] is `None` in both cases.
    #[must_use]
    pub fn from_config(dod: &DodSection, providers: &HashMap<String, ProviderToml>) -> Self {
        let triage = &dod.triage;
        let provider = triage
            .provider_and_model()
            .map(|(provider, _)| provider.to_owned());
        let locality = provider
            .as_deref()
            .map(|provider| provider_locality(provider, providers));
        let provider_enabled = provider
            .as_deref()
            .and_then(|provider| providers.get(provider))
            .map(|entry| entry.enabled);
        Self {
            model: triage.model.clone(),
            provider,
            locality,
            provider_enabled,
            effort: triage.effort.clone(),
            max_tokens: triage.max_tokens,
            max_parallel: triage.max_parallel,
            watch_mode: dod.watch.mode,
            effective_watch_mode: dod.effective_watch_mode(),
            autoconnect: dod.watch.autoconnect,
            tui_tty: dod.approve.tui_tty,
        }
    }

    /// The notes every view repeats while the watch asks for triage:
    /// [`NO_TRIAGE_MODEL_NOTE`] without a model,
    /// [`TRIAGE_PROVIDER_MISSING_NOTE`] or [`TRIAGE_PROVIDER_DISABLED_NOTE`]
    /// when the provider of the model is not configured or disabled.
    #[must_use]
    pub fn notes(&self) -> Vec<&'static str> {
        let mut notes = Vec::new();
        if self.watch_mode == DodWatchMode::Triage {
            if self.model.is_none() {
                notes.push(NO_TRIAGE_MODEL_NOTE);
            }
            notes.extend(self.provider_block());
        }
        notes
    }

    // Why the provider of the triage model keeps triage from running: not
    // configured or disabled; `None` without a model or with an enabled
    // provider.
    fn provider_block(&self) -> Option<&'static str> {
        if self.model.is_none() {
            return None;
        }
        match self.provider_enabled {
            None => Some(TRIAGE_PROVIDER_MISSING_NOTE),
            Some(false) => Some(TRIAGE_PROVIDER_DISABLED_NOTE),
            Some(true) => None,
        }
    }

    /// Structured form, mirroring the config tables.
    ///
    /// # Returns
    /// `{"triage": {model, provider, effort, max_tokens, max_parallel,
    /// locality, provider_enabled}, "watch": {mode, effective_config,
    /// autoconnect}, "approve": {tui_tty, note}, "notes": [..]}`; `locality`
    /// is `local`, `remote`, `unknown` or `null` (no model),
    /// `provider_enabled` is `null` without a configured provider,
    /// `effective_config` is [`Self::effective_watch_mode`] (config-only, the
    /// egress gate is the doctor's), `approve.note` is [`TUI_TTY_NOTE`] while
    /// `tui_tty` is on, else `null`. Strings are escaped and bounded.
    #[must_use]
    pub fn view(&self) -> Value {
        json!({
            "triage": {
                "model": self.model.as_deref().map(bounded),
                "provider": self.provider.as_deref().map(bounded),
                "effort": self.effort.as_deref().map(bounded),
                "max_tokens": self.max_tokens,
                "max_parallel": self.max_parallel,
                "locality": self.locality.map(TriageLocality::as_str),
                "provider_enabled": self.provider_enabled,
            },
            "watch": {
                "mode": self.watch_mode.as_str(),
                "effective_config": self.effective_watch_mode.as_str(),
                "autoconnect": self.autoconnect,
            },
            "approve": {
                "tui_tty": self.tui_tty,
                "note": self.tui_tty.then_some(TUI_TTY_NOTE),
            },
            "notes": self.notes(),
        })
    }
}

/// Locality of the provider `provider` among the configured `providers`.
///
/// # Returns
/// [`base_url_locality`] of its `base_url`; [`TriageLocality::Unknown`]
/// when no provider of that name is configured.
#[must_use]
pub fn provider_locality(
    provider: &str,
    providers: &HashMap<String, ProviderToml>,
) -> TriageLocality {
    providers
        .get(provider)
        .map_or(TriageLocality::Unknown, |entry| {
            base_url_locality(&entry.base_url)
        })
}

/// Locality of a provider `base_url`.
///
/// # Description
/// A URL with the scheme `unix`, `http+unix` or `https+unix`
/// (`unix:///run/x.sock`, `http+unix://%2Frun%2Fx.sock/v1`) is local, unless
/// the text after the scheme starts with a bare port (`unix:8080` is the
/// host `unix`). Otherwise the host is taken like
/// `harw_config::ProviderToml::is_local` does (scheme, userinfo, port and
/// IPv6 brackets removed, lower case, a trailing dot dropped), except that a
/// backslash also ends the authority, as in WHATWG URL parsing, so
/// `http://evil.example\@127.0.0.1` is not mistaken for loopback. Loopback
/// hosts (exactly `localhost`, `127.0.0.0/8`, `::1`, IPv4-mapped loopback)
/// are local. Unlike `ProviderToml::is_local`, a subdomain of `localhost`
/// (`foo.localhost`) counts as remote: without an RFC 6761 resolver (e.g.
/// nss-myhostname) glibc asks DNS for it, which may answer with an off-host
/// address, and this display must not call such an endpoint local (threat
/// B14). Private LAN addresses count as remote too.
///
/// # Arguments
/// - `base_url` (`&str`): the provider's `base_url`.
///
/// # Returns
/// [`TriageLocality::Local`], [`TriageLocality::Remote`] or, when no host
/// can be found, [`TriageLocality::Unknown`].
///
/// # Examples
/// ```rust
/// use harw_ops::dod_model::{TriageLocality, base_url_locality};
///
/// assert_eq!(base_url_locality("http://[::1]:8000/v1"), TriageLocality::Local);
/// assert_eq!(base_url_locality("unix:///run/llm.sock"), TriageLocality::Local);
/// assert_eq!(base_url_locality("http://10.0.0.5:8000"), TriageLocality::Remote);
/// assert_eq!(base_url_locality(""), TriageLocality::Unknown);
/// ```
#[must_use]
pub fn base_url_locality(base_url: &str) -> TriageLocality {
    let url = base_url.trim();
    if is_unix_socket_url(url) {
        return TriageLocality::Local;
    }
    match url_host(url) {
        Some(host) if host_is_loopback(&host) => TriageLocality::Local,
        Some(_) => TriageLocality::Remote,
        None => TriageLocality::Unknown,
    }
}

// Whether the URL names a Unix socket: the scheme (text before the first
// `:`) is exactly `unix`, `http+unix` or `https+unix` (any case), and the
// rest is non-empty and does not start with a bare port number (`unix:8080`
// is the scheme-less `host:port` form with the host `unix`).
fn is_unix_socket_url(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, rest)| {
        let socket_scheme = ["unix", "http+unix", "https+unix"]
            .iter()
            .any(|candidate| scheme.eq_ignore_ascii_case(candidate));
        let port_first = rest
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|head| !head.is_empty() && head.bytes().all(|byte| byte.is_ascii_digit()));
        socket_scheme && !rest.is_empty() && !port_first
    })
}

// Host part of a URL: without scheme, userinfo, port and IPv6 brackets,
// lower case, without a trailing dot; `None` when empty.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '\\', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next()?,
        None => authority.split(':').next()?,
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

// Exactly `localhost`, `127.0.0.0/8`, `::1` or IPv4-mapped loopback; never
// a subdomain of `localhost` (it may be resolved through DNS).
fn host_is_loopback(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(IpAddr::V6(v6)) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

// The usage error of `/dod model` (never echoes the arguments).
fn usage() -> OpError {
    OpError::InvalidArguments(format!("usage: {DOD_MODEL_USAGE}"))
}

/// Parses the arguments after `/dod model`.
///
/// # Arguments
/// - `args` (`&[&str]`): the trimmed, non-empty tokens after `model`, in
///   their original case.
///
/// # Returns
/// `[]`/`show` → [`DodModelCommand::Show`], `clear` →
/// [`DodModelCommand::Clear`], `set <provider>/<model>` →
/// [`DodModelCommand::Set`] with the validated reference (case kept). The
/// verbs are case-insensitive.
///
/// # Errors
/// [`OpError::InvalidArguments`] for anything else, including extra tokens
/// and an invalid reference ([`parse_model_ref`]).
pub fn parse_model_args(args: &[&str]) -> Result<DodModelCommand, OpError> {
    match args {
        [] => Ok(DodModelCommand::Show),
        [verb] if verb.eq_ignore_ascii_case("show") => Ok(DodModelCommand::Show),
        [verb] if verb.eq_ignore_ascii_case("clear") => Ok(DodModelCommand::Clear),
        [verb, model] if verb.eq_ignore_ascii_case("set") => {
            parse_model_ref(model).map(|model| DodModelCommand::Set(model.to_owned()))
        }
        _ => Err(usage()),
    }
}

/// Validates a triage model reference for `/dod model set`.
///
/// # Description
/// Trims `raw` and checks it with `harw_config::parse_triage_model`:
/// `<provider>/<model>`, printable ASCII without whitespace, at most
/// `harw_config::dod_toml::MAX_TRIAGE_MODEL_BYTES` bytes, no empty, `.` or
/// `..` segment, provider of ASCII letters, digits, `-`, `_`, `.`. Whether
/// provider and model exist is not checked.
///
/// # Returns
/// The trimmed reference (a sub-slice of `raw`).
///
/// # Errors
/// [`OpError::InvalidArguments`] with the reason, without echoing `raw`.
pub fn parse_model_ref(raw: &str) -> Result<&str, OpError> {
    let model = raw.trim();
    harw_config::parse_triage_model(model)
        .map(|_| model)
        .map_err(|reason| {
            // The reason may end in `, got "<input>"`; the input is not echoed.
            let reason = reason
                .split_once(", got ")
                .map_or(reason.as_str(), |(head, _)| head);
            OpError::InvalidArguments(format!(
                "invalid triage model: {}; usage: {DOD_MODEL_USAGE}",
                bounded(reason)
            ))
        })
}

/// Writes (`Some`) or removes (`None`) `[dod.triage] model` in
/// `<harw_home>/config.toml`.
///
/// # Description
/// Validates `model` with [`parse_model_ref`] first, then calls
/// `harw_config::set_dod_triage_model`, which keeps comments and all other
/// keys, validates the resulting `[dod]` table and writes atomically with
/// mode `0600`. The home directory is never created.
///
/// # Arguments
/// - `harw_home` (`&Path`): the harw home (root space), not the profile.
/// - `model` (`Option<&str>`): the new reference, or `None` to remove it.
///
/// # Returns
/// [`ModelWriteOutcome::Saved`], or [`ModelWriteOutcome::DurabilityUncertain`]
/// when the file was committed but the directory sync failed.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: invalid reference.
/// - [`OpError::NotAvailable`]: `harw_home` does not exist.
/// - [`OpError::Execution`]: another writer holds the lock or changed the
///   file (retry), or any other read/validation/write error (a fixed text;
///   the detail goes to `debug` only, it may contain paths).
///
/// # Concurrency
/// Blocking; serialized against other writers by the `ConfigWriter` lock.
pub fn write_model(harw_home: &Path, model: Option<&str>) -> Result<ModelWriteOutcome, OpError> {
    let model = model.map(parse_model_ref).transpose()?;
    match harw_config::set_dod_triage_model(harw_home, model) {
        Ok(()) => Ok(ModelWriteOutcome::Saved),
        Err(ConfigError::CommittedDurabilityUncertain(_)) => {
            Ok(ModelWriteOutcome::DurabilityUncertain)
        }
        Err(ConfigError::Invalid(message))
            if message == WRITER_BUSY || message == WRITER_CONFLICT =>
        {
            Err(OpError::Execution(message))
        }
        Err(ConfigError::Invalid(message)) if message == HOME_MISSING => Err(
            OpError::NotAvailable("the harw home does not exist; nothing was written".to_owned()),
        ),
        Err(error) => {
            tracing::debug!(error = %error, "dod model: writing [dod.triage] model failed");
            Err(OpError::Execution(WRITE_FAILED.to_owned()))
        }
    }
}

// One `label value` line of the text views.
fn text_row(label: &str, value: &str) -> String {
    format!("{label:<13} {value}")
}

/// Text form of `/dod model show` and `harw dod model show`.
#[must_use]
pub fn model_text(settings: &TriageSettings) -> String {
    let mut lines = vec!["DoD triage model ([dod.triage], PL-100 §3.2)".to_owned()];
    lines.push(text_row(
        "model",
        &settings
            .model
            .as_deref()
            .map_or_else(|| "(not set)".to_owned(), bounded),
    ));
    lines.push(text_row(
        "provider",
        &match (&settings.provider, settings.locality) {
            (Some(provider), Some(locality)) if settings.provider_enabled == Some(false) => {
                let provider = bounded(provider);
                format!(
                    "{provider} ({}; disabled, enable with `harw provider enable {provider}`)",
                    locality.label()
                )
            }
            (Some(provider), Some(locality)) => {
                format!("{} ({})", bounded(provider), locality.label())
            }
            _ => "-".to_owned(),
        },
    ));
    lines.push(text_row(
        "effort",
        &settings
            .effort
            .as_deref()
            .map_or_else(|| "(model default)".to_owned(), bounded),
    ));
    lines.push(text_row(
        "max_tokens",
        &settings
            .max_tokens
            .map_or_else(|| "(dispatcher default)".to_owned(), |n| n.to_string()),
    ));
    lines.push(text_row("max_parallel", &settings.max_parallel.to_string()));
    let watch = if settings.watch_mode != settings.effective_watch_mode {
        format!(
            "{} (effective {})",
            settings.watch_mode.as_str(),
            settings.effective_watch_mode.as_str()
        )
    } else if settings.effective_watch_mode == DodWatchMode::Triage
        && settings.provider_block().is_some()
    {
        // No triage without a usable provider; the note names the reason.
        format!(
            "{} (effective {})",
            settings.watch_mode.as_str(),
            DodWatchMode::Notify.as_str()
        )
    } else if settings.effective_watch_mode == DodWatchMode::Triage {
        // Config-only: the WP 0.3 egress gate is the doctor's.
        format!(
            "{} ({EGRESS_DOCTOR_HINT})",
            settings.effective_watch_mode.as_str()
        )
    } else {
        settings.watch_mode.as_str().to_owned()
    };
    lines.push(text_row("watch", &watch));
    for note in settings.notes() {
        lines.push(text_row("note", note));
    }
    lines.push(format!(
        "change with `{TRIAGE_MODEL_HINT}` or `/dod model clear`"
    ));
    lines.join("\n")
}

/// Structured form of `/dod model show`: [`TriageSettings::view`] plus
/// `"action": "show"`.
#[must_use]
pub fn model_view(settings: &TriageSettings) -> Value {
    let mut view = settings.view();
    if let Some(object) = view.as_object_mut() {
        object.insert("action".to_owned(), json!("show"));
    }
    view
}

/// Text and structured result of a successful `set` (`requested = Some`) or
/// `clear` (`None`).
///
/// # Arguments
/// - `requested` (`Option<&str>`): the reference written, or `None`.
/// - `outcome` (`ModelWriteOutcome`): from [`write_model`].
/// - `effective` (`Option<&TriageSettings>`): the settings re-read after the
///   write; `None` when the configuration could not be re-read.
///
/// # Returns
/// `(text, json)`. The JSON is `{"action": "set"|"clear", "ok": true,
/// "requested", "overridden", "warning"?, "effective": view|null}`;
/// `overridden` is `true` when the effective model differs from the home
/// layer (the active profile sets its own), `warning` is
/// `"durability_uncertain"` for [`ModelWriteOutcome::DurabilityUncertain`].
#[must_use]
pub fn model_change_output(
    requested: Option<&str>,
    outcome: ModelWriteOutcome,
    effective: Option<&TriageSettings>,
) -> (String, Value) {
    let action = if requested.is_some() { "set" } else { "clear" };
    let mut lines = vec![match requested {
        Some(model) => format!(
            "[dod.triage] model = {} written to the home config",
            bounded(model)
        ),
        None => "[dod.triage] model removed from the home config".to_owned(),
    }];
    let overridden = effective.is_some_and(|settings| settings.model.as_deref() != requested);
    if let Some(settings) = effective
        && overridden
    {
        lines.push(format!(
            "note: the effective [dod.triage] model is {} (the active profile takes precedence over the home config)",
            settings
                .model
                .as_deref()
                .map_or_else(|| "(not set)".to_owned(), bounded)
        ));
    }
    if outcome == ModelWriteOutcome::DurabilityUncertain {
        lines.push("warning: saved, but durability across a crash is uncertain".to_owned());
    }
    match effective {
        Some(settings) => {
            lines.push(String::new());
            lines.push(model_text(settings));
        }
        None => lines.push(format!(
            "note: the effective configuration could not be re-read; run `/dod model show` ({CONFIG_UNREADABLE})"
        )),
    }
    let mut view = json!({
        "action": action,
        "ok": true,
        "requested": requested.map(bounded),
        "overridden": overridden,
        "effective": effective.map(TriageSettings::view),
    });
    if outcome == ModelWriteOutcome::DurabilityUncertain
        && let Some(object) = view.as_object_mut()
    {
        object.insert("warning".to_owned(), json!("durability_uncertain"));
    }
    (lines.join("\n"), view)
}

/// The configuration of the trusted DoD layers: the harw home and the
/// active profile, never a project.
///
/// # Description
/// `[dod.triage]` counts only from these two layers (threat B15), and the
/// provider of the triage model is looked up among their `providers/*.toml`
/// as well, so a project cannot change the provider locality the doctor and
/// `/dod model show` display (threat B14). They are also the layers a
/// per-host `harw-dod-watch` has (it runs without a project). A project's
/// narrowing `[dod.approve] tui_tty = false` applies only inside that
/// project's sessions and is not part of this view. Nothing is created;
/// missing layers are skipped by `harw_config::discover_config`.
///
/// # Arguments
/// - `harw_home` (`&Path`): the harw home (root space).
/// - `profile_dir` (`&Path`): the directory of the active profile
///   (`<harw_home>/profiles/<name>`).
///
/// # Returns
/// The configuration resolved from `[harw_home, profile_dir]`.
///
/// # Errors
/// The [`ConfigError`] of `harw_config::discover_config`. It may quote
/// configuration text or paths: show a fixed text instead.
///
/// # Concurrency
/// Blocking file I/O on the calling thread.
pub fn trusted_config(harw_home: &Path, profile_dir: &Path) -> Result<ResolvedConfig, ConfigError> {
    let layers: [PathBuf; 2] = [harw_home.to_path_buf(), profile_dir.to_path_buf()];
    harw_config::discover_config(&layers)
}

/// [`TriageSettings`] of the trusted DoD layers ([`trusted_config`]).
///
/// # Errors
/// As [`trusted_config`].
///
/// # Examples
/// ```rust
/// use std::path::Path;
///
/// use harw_ops::dod_model::trusted_triage_settings;
///
/// // Missing layers are skipped: the defaults remain.
/// let home = Path::new("/nonexistent/harw-dod-model-doc");
/// let settings = trusted_triage_settings(home, &home.join("profiles/default"))?;
/// assert_eq!(settings.model, None);
/// # Ok::<(), harw_config::ConfigError>(())
/// ```
pub fn trusted_triage_settings(
    harw_home: &Path,
    profile_dir: &Path,
) -> Result<TriageSettings, ConfigError> {
    trusted_config(harw_home, profile_dir)
        .map(|config| TriageSettings::from_config(&config.dod, &config.providers))
}

/// [`trusted_triage_settings`] of the bound home and its profile; the
/// configuration error goes to `debug` only (it may name paths).
///
/// # Errors
/// [`OpError::Execution`] with a fixed text when the configuration cannot
/// be read.
pub(crate) fn bound_triage_settings(
    home: &harw_home::ResolvedHomeContext,
) -> Result<TriageSettings, OpError> {
    trusted_triage_settings(&home.home, &home.profile_dir).map_err(|error| {
        tracing::debug!(error = %error, "dod model: config discovery failed");
        OpError::Execution(CONFIG_UNREADABLE.to_owned())
    })
}

/// Runs a parsed `/dod model` subcommand against the bound harw home.
///
/// # Description
/// `show` re-reads the trusted DoD layers of the bound home
/// ([`trusted_config`]). `set`/`clear` write the home layer
/// ([`write_model`]), re-read those layers, mirror the effective `[dod.triage]
/// model` into the live configuration (`crate::live_config::mirror`) and
/// report a precedence of the active profile.
///
/// # Errors
/// - [`OpError::NotAvailable`]: no harw home is bound to the session, or it
///   does not exist.
/// - [`OpError::InvalidArguments`], [`OpError::Execution`]: see
///   [`write_model`]; `show` also fails with [`OpError::Execution`] on an
///   unreadable configuration.
///
/// # Concurrency
/// Blocking file I/O on the calling task, like `/settings`.
pub(crate) fn model_command(
    ctx: &OpContext,
    command: &DodModelCommand,
) -> Result<OpOutput, OpError> {
    let home = crate::config_util::bound_home(ctx)?;
    let model = match command {
        DodModelCommand::Show => {
            let settings = bound_triage_settings(home)?;
            return Ok(OpOutput {
                text: model_text(&settings),
                data: Some(model_view(&settings)),
            });
        }
        DodModelCommand::Set(model) => Some(model.as_str()),
        DodModelCommand::Clear => None,
    };
    let outcome = write_model(&home.home, model)?;
    let action = if model.is_some() { "set" } else { "clear" };
    tracing::info!(action, layer = "home", "dod triage model updated");
    // `[dod.triage]` comes only from home and profile, so the trusted view's
    // model is also the session's.
    let settings = bound_triage_settings(home).ok();
    if let Some(settings) = &settings {
        crate::live_config::mirror(ctx, |live| {
            live.dod.triage.model.clone_from(&settings.model);
        });
    }
    let (text, data) = model_change_output(model, outcome, settings.as_ref());
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn providers(entries: &[(&str, &str)]) -> TestResult<HashMap<String, ProviderToml>> {
        let mut map = HashMap::new();
        for (name, base_url) in entries {
            let provider: ProviderToml = toml::from_str(&format!(
                "name = \"{name}\"\napi = \"openai\"\nbase_url = \"{base_url}\"\n"
            ))
            .map_err(ctx("provider toml"))?;
            map.insert((*name).to_owned(), provider);
        }
        Ok(map)
    }

    fn invalid_message(result: Result<DodModelCommand, OpError>) -> TestResult<String> {
        match result {
            Err(OpError::InvalidArguments(message)) => Ok(message),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn parse_accepts_show_set_clear_and_keeps_the_model_case() -> TestResult {
        assert_eq!(
            parse_model_args(&[]).map_err(ctx("empty"))?,
            DodModelCommand::Show
        );
        assert_eq!(
            parse_model_args(&["SHOW"]).map_err(ctx("show"))?,
            DodModelCommand::Show
        );
        assert_eq!(
            parse_model_args(&["clear"]).map_err(ctx("clear"))?,
            DodModelCommand::Clear
        );
        assert_eq!(
            parse_model_args(&["set", "local/qwen2.5"]).map_err(ctx("local"))?,
            DodModelCommand::Set("local/qwen2.5".to_owned())
        );
        assert_eq!(
            parse_model_args(&["Set", "openrouter/openai/GPT-4o"]).map_err(ctx("nested"))?,
            DodModelCommand::Set("openrouter/openai/GPT-4o".to_owned())
        );
        Ok(())
    }

    #[test]
    fn parse_rejects_bad_forms_and_bad_references_without_echoing_them() -> TestResult {
        let too_long = format!("p/{}", "m".repeat(300));
        let bidi = "local/qwen\u{202E}2.5";
        let cases: Vec<Vec<&str>> = vec![
            vec!["set"],
            vec!["set", "noslash"],
            vec!["set", "provider/"],
            vec!["set", "/model"],
            vec!["set", "./x"],
            vec!["set", "../x"],
            vec!["set", "p/../x"],
            vec!["set", "p/a//b"],
            vec!["set", "p/x y"],
            vec!["set", bidi],
            vec!["set", too_long.as_str()],
            vec!["set", "a/b", "c"],
            vec!["clear", "x"],
            vec!["show", "x"],
            vec!["frobnicate"],
        ];
        for case in cases {
            let message = invalid_message(parse_model_args(&case))?;
            assert!(message.contains(DOD_MODEL_USAGE), "{case:?}: {message}");
            assert!(!message.contains("noslash"), "{case:?}: {message}");
            assert!(!message.contains('\u{202E}'), "{case:?}: {message}");
            assert!(!message.contains(&too_long), "{case:?}: {message}");
        }
        Ok(())
    }

    #[test]
    fn locality_is_local_only_for_loopback_and_unix_sockets() {
        for url in [
            "http://127.0.0.1:11434/v1",
            "http://127.1.2.3",
            "http://localhost:8080/v1",
            "HTTP://LOCALHOST./v1",
            "http://[::1]:8000/v1",
            "http://[::ffff:127.0.0.1]:8000",
            "http://user:pw@127.0.0.1:1/v1",
            "localhost:11434",
            "unix:///run/llm.sock",
            "UNIX:/run/llm.sock",
            "http+unix://%2Frun%2Fllm.sock/v1",
            "https+unix://%2Frun%2Fllm.sock/v1",
        ] {
            assert_eq!(base_url_locality(url), TriageLocality::Local, "{url}");
        }
        for url in [
            "https://openrouter.ai/api/v1",
            "https://api.example.com",
            "http://10.0.0.5:8000",
            "http://192.168.1.2:11434",
            "http://0.0.0.0:8000",
            "http://localhost.example.com",
            // A `localhost` subdomain may be resolved through DNS (no RFC
            // 6761 resolver) and is never shown as local.
            "http://foo.localhost/v1",
            "http://FOO.LOCALHOST.:8080/v1",
            "http://evil.example\\@127.0.0.1/v1",
            "http://127.0.0.1.example.com",
            // Only `unix`, `http+unix` and `https+unix` name a socket.
            "unix+https://api.example.com/v1",
            "unix+http://api.example.com/v1",
            "x-unix://api.example.com/v1",
            // Scheme-less `host:port` with the host `unix`.
            "unix:8080",
            "unix:8080/v1",
        ] {
            assert_eq!(base_url_locality(url), TriageLocality::Remote, "{url}");
        }
        for url in ["", "   ", "http://", "http:///v1"] {
            assert_eq!(base_url_locality(url), TriageLocality::Unknown, "{url}");
        }
        assert_eq!(TriageLocality::Local.label(), "lokal");
        assert_eq!(TriageLocality::Remote.label(), "remote");
        assert_eq!(TriageLocality::Unknown.label(), "unbekannt");
    }

    #[test]
    fn provider_locality_resolves_the_configured_provider_or_is_unknown() -> TestResult {
        let map = providers(&[
            ("local", "http://127.0.0.1:11434/v1"),
            ("openrouter", "https://openrouter.ai/api/v1"),
        ])?;
        assert_eq!(provider_locality("local", &map), TriageLocality::Local);
        assert_eq!(
            provider_locality("openrouter", &map),
            TriageLocality::Remote
        );
        assert_eq!(provider_locality("missing", &map), TriageLocality::Unknown);
        Ok(())
    }

    #[test]
    fn settings_without_a_model_state_the_notify_fallback() -> TestResult {
        let settings = TriageSettings::from_config(&DodSection::default(), &HashMap::new());
        assert_eq!(settings.model, None);
        assert_eq!(settings.locality, None);
        assert_eq!(settings.watch_mode, DodWatchMode::Triage);
        assert_eq!(settings.effective_watch_mode, DodWatchMode::Notify);
        assert!(settings.tui_tty);
        let view = settings.view();
        assert_eq!(view["triage"]["model"], Value::Null);
        assert_eq!(view["triage"]["locality"], Value::Null);
        assert_eq!(view["triage"]["provider_enabled"], Value::Null);
        assert_eq!(view["watch"]["mode"], "triage");
        assert_eq!(view["watch"]["effective_config"], "notify");
        assert!(view["watch"].get("effective").is_none());
        assert_eq!(view["approve"]["tui_tty"], true);
        assert_eq!(view["approve"]["note"], TUI_TTY_NOTE);
        assert_eq!(view["notes"], json!([NO_TRIAGE_MODEL_NOTE]));
        let text = model_text(&settings);
        assert!(text.contains("(not set)"), "{text}");
        assert!(text.contains(NO_TRIAGE_MODEL_NOTE), "{text}");
        assert!(text.contains("triage (effective notify)"), "{text}");
        assert!(!text.contains(EGRESS_DOCTOR_HINT), "{text}");

        let mut off = DodSection::default();
        off.watch.mode = DodWatchMode::Off;
        off.approve.tui_tty = false;
        let off = TriageSettings::from_config(&off, &HashMap::new());
        assert!(off.notes().is_empty());
        assert_eq!(off.view()["approve"]["note"], Value::Null);
        Ok(())
    }

    #[test]
    fn settings_with_a_model_show_provider_and_locality() -> TestResult {
        let mut dod = DodSection::default();
        dod.triage.model = Some("local/qwen2.5".to_owned());
        dod.triage.effort = Some("high".to_owned());
        dod.triage.max_tokens = Some(4096);
        let map = providers(&[("local", "http://127.0.0.1:11434/v1")])?;
        let settings = TriageSettings::from_config(&dod, &map);
        assert_eq!(settings.provider.as_deref(), Some("local"));
        assert_eq!(settings.locality, Some(TriageLocality::Local));
        assert_eq!(settings.provider_enabled, Some(true));
        assert_eq!(settings.effective_watch_mode, DodWatchMode::Triage);
        assert!(settings.notes().is_empty());
        let view = model_view(&settings);
        assert_eq!(view["action"], "show");
        assert_eq!(view["triage"]["model"], "local/qwen2.5");
        assert_eq!(view["triage"]["locality"], "local");
        assert_eq!(view["triage"]["provider_enabled"], true);
        assert_eq!(view["triage"]["max_tokens"], 4096);
        assert_eq!(view["watch"]["effective_config"], "triage");
        let text = model_text(&settings);
        assert!(text.contains("local (lokal)"), "{text}");
        assert!(!text.contains("disabled"), "{text}");
        assert!(text.contains("4096"), "{text}");
        // A config-only `triage` points at the doctor's egress check.
        assert!(
            text.contains(&format!("triage ({EGRESS_DOCTOR_HINT})")),
            "{text}"
        );

        dod.triage.model = Some("cloud/m".to_owned());
        let unknown = TriageSettings::from_config(&dod, &map);
        assert_eq!(unknown.locality, Some(TriageLocality::Unknown));
        assert_eq!(unknown.provider_enabled, None);
        // Without a `[providers]` entry triage cannot run: notify, no fallback.
        assert_eq!(unknown.notes(), vec![TRIAGE_PROVIDER_MISSING_NOTE]);
        assert_eq!(
            unknown.view()["notes"],
            json!([TRIAGE_PROVIDER_MISSING_NOTE])
        );
        let text = model_text(&unknown);
        assert!(text.contains("cloud (unbekannt)"), "{text}");
        assert!(text.contains("triage (effective notify)"), "{text}");
        assert!(text.contains(TRIAGE_PROVIDER_MISSING_NOTE), "{text}");
        assert!(!text.contains(EGRESS_DOCTOR_HINT), "{text}");

        // Without a triage request the missing provider blocks nothing.
        dod.watch.mode = DodWatchMode::Notify;
        let quiet = TriageSettings::from_config(&dod, &map);
        assert!(quiet.notes().is_empty());
        assert!(!model_text(&quiet).contains("effective"));
        Ok(())
    }

    /// A catalog-seeded provider starts with `enabled = false`: every view
    /// names it and the command that enables it.
    #[test]
    fn settings_flag_a_disabled_triage_provider() -> TestResult {
        let provider: ProviderToml = toml::from_str(
            "name = \"lmstudio\"\napi = \"openai\"\nbase_url = \"http://127.0.0.1:1234/v1\"\nenabled = false\n",
        )
        .map_err(ctx("provider toml"))?;
        let map = HashMap::from([("lmstudio".to_owned(), provider)]);
        let mut dod = DodSection::default();
        dod.triage.model = Some("lmstudio/qwen".to_owned());
        let settings = TriageSettings::from_config(&dod, &map);
        assert_eq!(settings.provider_enabled, Some(false));
        assert_eq!(settings.locality, Some(TriageLocality::Local));
        assert_eq!(settings.view()["triage"]["provider_enabled"], false);
        let text = model_text(&settings);
        assert!(
            text.contains(
                "lmstudio (lokal; disabled, enable with `harw provider enable lmstudio`)"
            ),
            "{text}"
        );
        assert_eq!(settings.notes(), vec![TRIAGE_PROVIDER_DISABLED_NOTE]);
        assert!(text.contains("triage (effective notify)"), "{text}");
        assert!(text.contains(TRIAGE_PROVIDER_DISABLED_NOTE), "{text}");
        Ok(())
    }

    /// Threat B15/B14: the trusted view reads home and profile only, so a
    /// further (project) layer that redefines the triage provider does not
    /// change the locality shown.
    #[test]
    fn trusted_settings_ignore_a_project_provider_of_the_same_name() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = dir.path().join("home");
        let profile = home.join("profiles").join("default");
        let project = dir.path().join("repo").join(".harw");
        for (layer, base_url) in [
            (&home, "https://api.example.com/v1"),
            (&project, "http://127.0.0.1:11434/v1"),
        ] {
            std::fs::create_dir_all(layer.join("providers")).map_err(ctx("providers"))?;
            std::fs::write(
                layer.join("providers").join("p.toml"),
                format!("name = \"p\"\napi = \"openai\"\nbase_url = \"{base_url}\"\n"),
            )
            .map_err(ctx("provider"))?;
        }
        std::fs::create_dir_all(&profile).map_err(ctx("profile"))?;
        std::fs::write(
            profile.join("config.toml"),
            "[dod.triage]\nmodel = \"p/m\"\n",
        )
        .map_err(ctx("profile config"))?;

        let with_project =
            harw_config::discover_config(&[home.clone(), profile.clone(), project.clone()])
                .map_err(ctx("discover with project"))?;
        assert_eq!(
            provider_locality("p", &with_project.providers),
            TriageLocality::Local,
            "precondition: the project layer redefines `p`"
        );
        let trusted = trusted_triage_settings(&home, &profile).map_err(ctx("trusted"))?;
        assert_eq!(trusted.model.as_deref(), Some("p/m"));
        assert_eq!(trusted.locality, Some(TriageLocality::Remote));
        Ok(())
    }

    #[test]
    fn write_model_sets_and_clears_the_home_layer_only() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# keep me\n[tui]\nlanguage = 'de'\n").map_err(ctx("seed"))?;
        assert_eq!(
            write_model(dir.path(), Some(" local/Qwen2.5 ")).map_err(ctx("set"))?,
            ModelWriteOutcome::Saved
        );
        let written = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        assert!(written.contains("# keep me"), "{written}");
        assert!(written.contains("model = \"local/Qwen2.5\""), "{written}");
        write_model(dir.path(), None).map_err(ctx("clear"))?;
        let cleared = std::fs::read_to_string(&path).map_err(ctx("read cleared"))?;
        assert!(!cleared.contains("local/Qwen2.5"), "{cleared}");
        assert!(cleared.contains("language = 'de'"), "{cleared}");
        Ok(())
    }

    #[test]
    fn write_model_rejects_bad_input_and_a_missing_home_without_writing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        let seed = "[dod.triage]\nmodel = \"local/keep\"\n";
        std::fs::write(&path, seed).map_err(ctx("seed"))?;
        assert!(matches!(
            write_model(dir.path(), Some("noslash")),
            Err(OpError::InvalidArguments(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).map_err(ctx("read"))?, seed);
        assert!(matches!(
            write_model(&dir.path().join("missing"), Some("local/x")),
            Err(OpError::NotAvailable(_))
        ));
        assert!(!dir.path().join("missing").exists());
        std::fs::write(&path, "[dod]\ntriage = 'not a table'\n").map_err(ctx("bad"))?;
        match write_model(dir.path(), Some("local/x")) {
            Err(OpError::Execution(message)) => {
                assert_eq!(message, WRITE_FAILED);
                assert!(!message.contains("not a table"));
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    /// `write_model` recognizes harw-config's busy text by equality; this
    /// pins the coupling: a held `ConfigWriter` lock yields the retry hint,
    /// not the generic write failure, and nothing is written.
    #[test]
    fn write_model_reports_a_held_writer_lock_as_busy() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        let seed = "[dod.triage]\nmodel = \"local/keep\"\n";
        std::fs::write(&path, seed).map_err(ctx("seed"))?;
        let held = harw_config::ConfigWriter::open_locked(&path).map_err(ctx("lock"))?;
        match write_model(dir.path(), Some("local/x")) {
            Err(OpError::Execution(message)) => assert_eq!(message, WRITER_BUSY),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert_eq!(std::fs::read_to_string(&path).map_err(ctx("read"))?, seed);
        drop(held);
        write_model(dir.path(), Some("local/x")).map_err(ctx("after unlock"))?;
        Ok(())
    }

    #[test]
    fn change_output_reports_profile_precedence_and_durability() -> TestResult {
        let mut dod = DodSection::default();
        dod.triage.model = Some("profile/model".to_owned());
        let effective = TriageSettings::from_config(&dod, &HashMap::new());
        let (text, view) = model_change_output(
            Some("local/x"),
            ModelWriteOutcome::DurabilityUncertain,
            Some(&effective),
        );
        assert!(text.contains("local/x written"), "{text}");
        assert!(
            text.contains("effective [dod.triage] model is profile/model"),
            "{text}"
        );
        assert!(text.contains("durability"), "{text}");
        assert_eq!(view["action"], "set");
        assert_eq!(view["overridden"], true);
        assert_eq!(view["warning"], "durability_uncertain");
        assert_eq!(view["effective"]["triage"]["model"], "profile/model");

        let cleared = TriageSettings::from_config(&DodSection::default(), &HashMap::new());
        let (text, view) = model_change_output(None, ModelWriteOutcome::Saved, Some(&cleared));
        assert!(text.contains("removed"), "{text}");
        assert!(text.contains(NO_TRIAGE_MODEL_NOTE), "{text}");
        assert_eq!(view["action"], "clear");
        assert_eq!(view["overridden"], false);
        assert!(view.get("warning").is_none());

        let (text, view) = model_change_output(None, ModelWriteOutcome::Saved, None);
        assert!(text.contains("could not be re-read"), "{text}");
        assert_eq!(view["effective"], Value::Null);
        Ok(())
    }
}
