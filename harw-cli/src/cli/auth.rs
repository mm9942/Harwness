//! Grammatik von `harw auth`: Credential-Verwaltung.

use clap::Subcommand;

use super::values::{ImportSource, LoginProvider, TokenProvider};

/// Aktionen des `harw auth`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthAction {
    /// Setup-Token per PKCE-Paste-Flow beschaffen und hinterlegen.
    ///
    /// Zeigt die Authorize-URL, nimmt den zurückgegebenen Code auf `stdin`
    /// entgegen, tauscht ihn gegen den Token, legt ihn verschlüsselt im
    /// SecretStore ab und zeigt die `secrets:`-Referenz an (nie den Wert).
    Login {
        /// Provider (derzeit nur `anthropic`).
        #[arg(value_enum, default_value_t = LoginProvider::Anthropic)]
        provider: LoginProvider,
    },
    /// Setup-Token direkt setzen (Alternative zum API-Key). Liest von `stdin`.
    ///
    /// Legt den Token verschlüsselt im SecretStore ab und zeigt die
    /// `secrets:`-Referenz an (nie den Wert).
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
    /// Klartext-Secret-Referenzen in den verschlüsselten SecretStore überführen.
    ///
    /// Deckt `auth` in `<home>/providers/*.toml` und `<profile>/providers/*.toml`
    /// sowie `credential_pool`-Einträge in `<home>/auth.toml` und
    /// `<profile>/auth.toml` ab: `file:`/`file-json:`/`env:` werden kopiert und
    /// auf `secrets:<id>` umgeschrieben; Codex-Login-Verweise bleiben. Nur von
    /// Harw angelegte Dateien unter `<home>/secrets/` werden gelöscht. Gibt nie
    /// einen Wert aus.
    Migrate {
        /// Nur den Plan anzeigen, nichts ändern.
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{AuthAction, Cli, Command};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn auth_migrate_parses_with_and_without_dry_run() -> TestResult {
        let plain =
            Cli::try_parse_from(["harw", "auth", "migrate"]).map_err(ctx("auth migrate"))?;
        assert!(
            matches!(
                plain.command,
                Some(Command::Auth {
                    action: AuthAction::Migrate { dry_run: false },
                    ..
                })
            ),
            "{:?}",
            plain.command
        );
        let dry = Cli::try_parse_from(["harw", "auth", "migrate", "--dry-run"])
            .map_err(ctx("auth migrate --dry-run"))?;
        assert!(
            matches!(
                dry.command,
                Some(Command::Auth {
                    action: AuthAction::Migrate { dry_run: true },
                    ..
                })
            ),
            "{:?}",
            dry.command
        );
        Ok(())
    }
}
