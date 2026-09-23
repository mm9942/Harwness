//! Typisierte Werte der `harw`-Grammatik.
//!
//! Enthält die `ValueEnum`s für Argumente mit fester Wertemenge sowie zwei
//! Wert-Parser für frei formulierte, aber prüfbare Werte:
//! [`LogFilterParser`] (`--log`, `tracing-subscriber`-Filtersyntax) und
//! [`ModeParser`] (`--mode`, Modusliste aus `harw-core`). Beide melden ihre
//! möglichen Werte an clap, sodass Hilfe und Shell-Completions sie anzeigen.
//!
//! Jedes Enum liefert über `as_str` genau die Zeichenkette, die die
//! aufrufenden Kommando-Module vor der Typisierung als `String` erhielten.

use std::ffi::OsStr;
use std::fmt;

use clap::builder::{PossibleValue, TypedValueParser};
use clap::error::ErrorKind;
use clap::{Arg, Command, ValueEnum};
use harw_core::mode::InteractionMode;

/// Implementiert `Display` für ein Enum durch Delegation an `as_str`.
macro_rules! display_via_as_str {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl fmt::Display for $ty {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str(self.as_str())
                }
            }
        )+
    };
}

/// Ein-/Aus-Schalter (`on` | `off`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    /// Einschalten.
    #[value(name = "on")]
    On,
    /// Ausschalten.
    #[value(name = "off")]
    Off,
}

impl OnOff {
    /// Liefert den kanonischen Namen (`"on"` | `"off"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// Standard-Freigabemodus (`ask` | `auto` | `full`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PermissionMode {
    /// Vor jeder freigabepflichtigen Aktion nachfragen.
    #[value(name = "ask")]
    Ask,
    /// Automatisch freigeben, soweit die Regeln es erlauben.
    #[value(name = "auto")]
    Auto,
    /// Volle Freigabe.
    #[value(name = "full")]
    Full,
}

impl PermissionMode {
    /// Liefert den kanonischen Namen (`"ask"` | `"auto"` | `"full"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Full => "full",
        }
    }
}

/// Quellmenge für `harw lens build --source` (`docs` | `knowledge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LensSource {
    /// Design-/Architekturdokumente aus `docs/`.
    #[value(name = "docs")]
    Docs,
    /// Memory-Palace-Artefakte des aktiven Profils.
    #[value(name = "knowledge")]
    Knowledge,
}

impl LensSource {
    /// Liefert den kanonischen Namen (`"docs"` | `"knowledge"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docs => "docs",
            Self::Knowledge => "knowledge",
        }
    }
}

/// Bereich für `harw uninstall --scope` (`service` | `state` | `workspace` | `binary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UninstallScope {
    /// Hintergrunddienst-Unit.
    #[value(name = "service")]
    Service,
    /// Zustandsdaten.
    #[value(name = "state")]
    State,
    /// Arbeitsbereich.
    #[value(name = "workspace")]
    Workspace,
    /// Installiertes Binary.
    #[value(name = "binary")]
    Binary,
}

impl UninstallScope {
    /// Liefert den kanonischen Namen (`"service"`, `"state"`, `"workspace"`, `"binary"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::State => "state",
            Self::Workspace => "workspace",
            Self::Binary => "binary",
        }
    }
}

/// Channel-Art für `harw connect --channel` (derzeit nur `telegram`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Channel {
    /// Telegram-Bot.
    #[value(name = "telegram")]
    Telegram,
}

impl Channel {
    /// Liefert den kanonischen Namen (`"telegram"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
        }
    }
}

/// Provider für `harw auth login` (derzeit nur `anthropic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoginProvider {
    /// Anthropic (Claude-Setup-Token per PKCE).
    #[value(name = "anthropic")]
    Anthropic,
}

impl LoginProvider {
    /// Liefert den kanonischen Namen (`"anthropic"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
        }
    }
}

/// Provider für `harw auth token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TokenProvider {
    /// Anthropic.
    #[value(name = "anthropic")]
    Anthropic,
    /// OpenAI.
    #[value(name = "openai")]
    Openai,
    /// Google Gemini.
    #[value(name = "gemini")]
    Gemini,
    /// Mistral.
    #[value(name = "mistral")]
    Mistral,
}

impl TokenProvider {
    /// Liefert den kanonischen Namen (`"anthropic"`, `"openai"`, `"gemini"`, `"mistral"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Gemini => "gemini",
            Self::Mistral => "mistral",
        }
    }
}

/// Quelle für `harw auth import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ImportSource {
    /// `~/.codex/auth.json` (OpenAI).
    #[value(name = "codex")]
    Codex,
    /// Codex-OAuth-Credentials.
    #[value(name = "codex-oauth")]
    CodexOauth,
    /// Claude-CLI-Credentials (Anthropic).
    #[value(name = "claude-cli")]
    ClaudeCli,
    /// Claude-Setup-Token.
    #[value(name = "claude-setup-token")]
    ClaudeSetupToken,
    /// Gemini-Key aus der Umgebung.
    #[value(name = "gemini-env")]
    GeminiEnv,
    /// Mistral-Key aus der Umgebung.
    #[value(name = "mistral-env")]
    MistralEnv,
}

impl ImportSource {
    /// Liefert den kanonischen Namen (z. B. `"codex"`, `"claude-cli"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::CodexOauth => "codex-oauth",
            Self::ClaudeCli => "claude-cli",
            Self::ClaudeSetupToken => "claude-setup-token",
            Self::GeminiEnv => "gemini-env",
            Self::MistralEnv => "mistral-env",
        }
    }
}

/// MCP-Server für `harw mcp setup|check` (derzeit nur `cloudflare`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpServer {
    /// Cloudflare-MCP (Alias `cloudflare-api`).
    #[value(name = "cloudflare", alias = "cloudflare-api")]
    Cloudflare,
}

impl McpServer {
    /// Liefert den kanonischen Namen (`"cloudflare"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloudflare => "cloudflare",
        }
    }
}

/// API-Dialekt für `harw settings provider add --api`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApiDialect {
    /// OpenAI Chat Completions.
    #[value(name = "openai-chat")]
    OpenaiChat,
    /// OpenAI Responses.
    #[value(name = "openai-responses")]
    OpenaiResponses,
    /// Anthropic Messages.
    #[value(name = "anthropic-messages")]
    AnthropicMessages,
    /// Ollama.
    #[value(name = "ollama")]
    Ollama,
}

impl ApiDialect {
    /// Liefert den kanonischen Namen (z. B. `"openai-chat"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai-chat",
            Self::OpenaiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
            Self::Ollama => "ollama",
        }
    }
}

display_via_as_str!(
    OnOff,
    PermissionMode,
    LensSource,
    UninstallScope,
    Channel,
    LoginProvider,
    TokenProvider,
    ImportSource,
    McpServer,
    ApiDialect,
);

/// Einfache Log-Level, die Hilfe und Completions für `--log` anbieten.
///
/// `--log` akzeptiert darüber hinaus jede gültige `tracing-subscriber`-
/// Filterdirektive (z. B. `harw_core=debug,info`).
pub const LOG_LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// Liest einen nicht-UTF-8-Wert als clap-Fehler statt als Panik.
fn value_to_str<'a>(cmd: &Command, value: &'a OsStr) -> Result<&'a str, clap::Error> {
    value.to_str().ok_or_else(|| {
        clap::Error::raw(
            ErrorKind::InvalidUtf8,
            format!("invalid UTF-8 in value '{}'\n", value.to_string_lossy()),
        )
        .with_cmd(cmd)
    })
}

/// Wert-Parser für `--log`: prüft die Filterdirektive schon beim Parsen.
///
/// # Description
/// Akzeptiert alles, was `tracing_subscriber::EnvFilter::try_new` annimmt,
/// und gibt den Wert unverändert zurück. Ungültige Filter werden als
/// [`ErrorKind::InvalidValue`] gemeldet. Als mögliche Werte meldet der Parser
/// [`LOG_LEVELS`] (für Hilfe und Shell-Completions).
#[derive(Debug, Clone, Copy, Default)]
pub struct LogFilterParser;

impl TypedValueParser for LogFilterParser {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &Command,
        _arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let raw = value_to_str(cmd, value)?;
        match tracing_subscriber::EnvFilter::try_new(raw) {
            Ok(_) => Ok(raw.to_owned()),
            Err(error) => Err(clap::Error::raw(
                ErrorKind::InvalidValue,
                format!("invalid log filter '{raw}': {error}\n"),
            )
            .with_cmd(cmd)),
        }
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(LOG_LEVELS.into_iter().map(PossibleValue::new)))
    }
}

/// Wert-Parser für `--mode`: prüft den Modus gegen `harw-core`.
///
/// # Description
/// Liest den Wert über [`InteractionMode::parse`] (Groß-/Kleinschreibung,
/// Kebab-/Snake-Case, umgebende Leerzeichen egal) und liefert den
/// kanonischen Namen ([`InteractionMode::as_str`]). Ein unbekannter Modus ist
/// ein [`ErrorKind::InvalidValue`], der alle gültigen Namen aufzählt. Als
/// mögliche Werte meldet der Parser [`InteractionMode::names`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ModeParser;

impl TypedValueParser for ModeParser {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &Command,
        _arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let raw = value_to_str(cmd, value)?;
        match InteractionMode::parse(raw) {
            Some(mode) => Ok(mode.as_str().to_owned()),
            None => {
                let valid = InteractionMode::names().collect::<Vec<_>>().join(", ");
                Err(clap::Error::raw(
                    ErrorKind::InvalidValue,
                    format!("invalid mode '{raw}' (valid: {valid})\n"),
                )
                .with_cmd(cmd))
            }
        }
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(InteractionMode::names().map(PossibleValue::new)))
    }
}
