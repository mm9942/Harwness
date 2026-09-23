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
}
