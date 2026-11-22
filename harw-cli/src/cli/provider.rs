//! Grammatik von `harw provider`: Modell-Anbieter verwalten und abfragen.

use clap::{Subcommand, ValueHint};

use super::values::ApiDialect;

/// Anmeldeart für `harw provider add-known`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ProviderAuthKindArg {
    /// API-Schlüssel (nur schreiben, nie auslesen).
    #[value(name = "api-key")]
    ApiKey,
    /// OAuth bzw. Setup-Token des Abos.
    #[value(name = "oauth")]
    Oauth,
}

impl ProviderAuthKindArg {
    /// Abbildung auf den `harw-ops`-Typ.
    #[must_use]
    pub fn as_ops(self) -> harw_ops::provider::ProviderAuthKind {
        match self {
            Self::ApiKey => harw_ops::provider::ProviderAuthKind::ApiKey,
            Self::Oauth => harw_ops::provider::ProviderAuthKind::Oauth,
        }
    }
}

/// Aktionen des `harw provider`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ProviderAction {
    /// Listet alle Anbieter des aktiven Profils auf.
    List,
    /// Legt einen neuen Anbieter an oder überschreibt einen gleichnamigen.
    Add {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
        /// API-Dialekt, z. B. `openai-chat`, `openai-responses`,
        /// `anthropic-messages`, `ollama`.
        #[arg(long, value_enum)]
        api: ApiDialect,
        /// Basis-URL des Anbieters.
        #[arg(long = "base-url", value_name = "URL", value_hint = ValueHint::Url)]
        base_url: String,
        /// Verweis auf das Geheimnis (`env:VAR` oder `secrets:NAME`); ein
        /// Schlüssel im Klartext wird abgelehnt.
        #[arg(long, value_hint = ValueHint::Other)]
        auth: Option<String>,
        /// Modell-IDs dieses Anbieters, kommagetrennt.
        #[arg(long, value_delimiter = ',', value_hint = ValueHint::Other)]
        models: Vec<String>,
        /// Credential-Transport: `bearer` (Standard bei Schlüssel), `x-api-key`,
        /// `api-key` oder `none`. Ohne Angabe: `none` für lokale Endpunkte
        /// ohne `--auth`, sonst die Vorgabe des Transports (Runde 7, Teil L1).
        #[arg(
            long = "auth-header",
            value_parser = ["bearer", "x-api-key", "api-key", "none"],
            conflicts_with = "no_auth"
        )]
        auth_header: Option<String>,
        /// Kein Schlüssel und kein Auth-Header, z. B. für einen lokalen
        /// vLLM- oder LM-Studio-Server (gleichbedeutend mit
        /// `--auth-header none` ohne `--auth`).
        #[arg(long = "no-auth", conflicts_with = "auth")]
        no_auth: bool,
        /// Erlaubt unverschlüsseltes `http` zu einer privaten LAN-IP
        /// (`10/8`, `172.16/12`, `192.168/16`); sonst nur Loopback.
        #[arg(long = "allow-insecure-lan")]
        allow_insecure_lan: bool,
    },
    /// Entfernt einen Anbieter.
    Remove {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Aktiviert einen zuvor deaktivierten Anbieter.
    Enable {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Deaktiviert einen Anbieter, ohne ihn zu löschen.
    Disable {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Zeigt die bekannten Anbieter des eingebauten Katalogs (Adresse fest).
    Catalog,
    /// Legt einen bekannten Anbieter aus dem Katalog an; Adresse und API sind fest.
    AddKnown {
        /// Katalog-Id, z. B. `anthropic` oder `openai`.
        #[arg(value_hint = ValueHint::Other)]
        catalog_id: String,
        /// Anmeldeart; `api-key` und `oauth` können parallel als zwei Einträge bestehen.
        #[arg(long = "auth-kind", value_enum, default_value = "api-key")]
        auth_kind: ProviderAuthKindArg,
        /// Name des Eintrags; Standard `<id>-key` bzw. `<id>-oauth`.
        #[arg(long, value_hint = ValueHint::Other)]
        name: Option<String>,
        /// Schlüssel bzw. Setup-Token verdeckt von stdin lesen (nie als Argument).
        #[arg(long = "key-stdin")]
        key_stdin: bool,
    },
    /// Fragt die verfügbaren Modelle der Anbieter ab und übernimmt sie.
    Scan {
        /// Nur diesen Anbieter abfragen; ohne Angabe alle aktivierten.
        #[arg(value_hint = ValueHint::Other)]
        provider: Option<String>,
        /// Nur kostenlose Modelle anzeigen und übernehmen.
        #[arg(long)]
        free_only: bool,
        /// Nicht mehr gemeldete Modelle aus der Auswahl entfernen.
        #[arg(long)]
        prune: bool,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};
    use crate::test_support::{TestError, TestResult, ctx};

    fn action(args: &[&str]) -> TestResult<ProviderAction> {
        let cli = Cli::try_parse_from(args).map_err(ctx("provider-Aufruf sollte parsen"))?;
        match cli.command {
            Some(Command::Provider { action }) => Ok(action),
            _ => Err(TestError::Unexpected(String::from("kein provider-Befehl"))),
        }
    }

    #[test]
    fn add_known_parses_default_api_key() -> TestResult {
        let a = action(&["harw", "provider", "add-known", "anthropic"])?;
        let ProviderAction::AddKnown { catalog_id, auth_kind, name, key_stdin } = a else {
            return Err(TestError::Unexpected(String::from("erwartete add-known")));
        };
        assert_eq!(catalog_id, "anthropic");
        assert_eq!(auth_kind, ProviderAuthKindArg::ApiKey);
        assert_eq!(name, None);
        assert!(!key_stdin);
        Ok(())
    }

    #[test]
    fn add_known_parses_oauth_and_name() -> TestResult {
        let a = action(&[
            "harw", "provider", "add-known", "anthropic", "--auth-kind", "oauth", "--name",
            "claude-sub", "--key-stdin",
        ])?;
        let ProviderAction::AddKnown { auth_kind, name, key_stdin, .. } = a else {
            return Err(TestError::Unexpected(String::from("erwartete add-known")));
        };
        assert_eq!(auth_kind, ProviderAuthKindArg::Oauth);
        assert_eq!(name.as_deref(), Some("claude-sub"));
        assert!(key_stdin);
        Ok(())
    }

    #[test]
    fn add_known_rejects_base_url_flag() -> TestResult {
        let r = Cli::try_parse_from([
            "harw", "provider", "add-known", "anthropic", "--base-url", "https://x.example.test",
        ]);
        assert!(r.is_err());
        Ok(())
    }

    #[test]
    fn add_known_rejects_api_and_auth_flags() -> TestResult {
        assert!(
            Cli::try_parse_from(["harw", "provider", "add-known", "openai", "--api", "openai-chat"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["harw", "provider", "add-known", "openai", "--auth", "env:X"])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn add_known_has_no_key_argument() -> TestResult {
        let r = Cli::try_parse_from(["harw", "provider", "add-known", "anthropic", "--key", "sk-test-FAKE"]);
        assert!(r.is_err());
        Ok(())
    }

    #[test]
    fn catalog_parses() -> TestResult {
        let a = action(&["harw", "provider", "catalog"])?;
        assert!(matches!(a, ProviderAction::Catalog));
        Ok(())
    }

    #[test]
    fn auth_kind_maps_to_ops() -> TestResult {
        assert_eq!(
            ProviderAuthKindArg::Oauth.as_ops(),
            harw_ops::provider::ProviderAuthKind::Oauth
        );
        assert_eq!(
            ProviderAuthKindArg::ApiKey.as_ops(),
            harw_ops::provider::ProviderAuthKind::ApiKey
        );
        Ok(())
    }
}
