//! Grammatik von `harw auth`: Credential-Verwaltung.

use clap::Subcommand;

use super::values::{ImportSource, LoginProvider, TokenProvider};

/// Aktionen des `harw auth`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthAction {
    /// Setup-Token per PKCE-Paste-Flow beschaffen und hinterlegen.
    ///
    /// Zeigt die Authorize-URL, nimmt den zurückgegebenen Code auf `stdin`
    /// entgegen, tauscht ihn gegen den Token, speichert ihn (0600) und druckt
    /// die `export CLAUDE_CODE_OAUTH_TOKEN=…`-Zeile.
    Login {
        /// Provider (derzeit nur `anthropic`).
        #[arg(value_enum, default_value_t = LoginProvider::Anthropic)]
        provider: LoginProvider,
    },
    /// Setup-Token direkt setzen (Alternative zum API-Key). Liest von `stdin`.
    Token {
        /// Provider (derzeit nur `anthropic`).
        #[arg(value_enum, default_value_t = TokenProvider::Anthropic)]
        provider: TokenProvider,
    },
    /// Lokale Credentials importieren (z. B. `~/.codex/auth.json`).
    Import {
        /// Quelle: `codex` (OpenAI) oder `claude-cli` (Anthropic).
        #[arg(value_enum, default_value_t = ImportSource::Codex)]
        source: ImportSource,
    },
    /// Vorhandene Credential-Quellen anzeigen (ohne Secrets).
    Status,
    /// Veraltete Credential-Pool-Einträge entfernen.
    ///
    /// Entfernt aus `credential_pool.<provider>` in `auth.toml` jeden Eintrag,
    /// der nicht zur Route des Providers passt (`provider.auth`/`base_url`:
    /// Codex-Login-Verweise nur auf der Codex-Route, alle anderen nur
    /// daneben) sowie doppelte Einträge — dieselbe Regel wie beim
    /// Onboarding. Ohne Provider werden alle Pools geprüft. Gibt nur Index
    /// und Label aus, nie ein Secret.
    Prune {
        /// Provider-Id (z. B. `openai`); ohne Angabe alle Pools.
        provider: Option<String>,
    },
}
