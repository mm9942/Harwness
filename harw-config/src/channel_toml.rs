//! Channel-Binding-Konfiguration (`channels/*.toml`).
//!
//! Telegram ist die erste konkrete Bindung (siehe
//! `docs/design/channel-ingress-telegram.md` §3.1). Weitere Channel-Arten
//! (Slack, E-Mail, ...) bekommen eigene `Vec<...>`-Felder in
//! `ChannelSectionToml`, ohne das bestehende Schema zu brechen.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auth_toml::SecretRef;
use crate::error::ConfigError;

/// Eine Datei unter `channels/*.toml`: `[[channel.telegram]]` (und künftig
/// weitere Channel-Arten) unter einer gemeinsamen `[channel]`-Sektion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelFileToml {
    #[serde(default)]
    pub channel: ChannelSectionToml,
}

/// Sammlung aller Channel-Bindungen einer Datei, nach Art gruppiert.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSectionToml {
    #[serde(default)]
    pub telegram: Vec<TelegramChannelToml>,
}

/// Eine konfigurierte Telegram-Bot-Bindung.
/// Siehe `docs/design/channel-ingress-telegram.md` §3.1 für die vollständige
/// Erklärung jedes Feldes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramChannelToml {
    /// `ChannelId`, z. B. `"telegram:support-bot"`.
    pub id: String,
    #[serde(default = "default_tenant_binding")]
    pub tenant_binding: String,
    /// Niemals ein literaler Token — immer eine `SecretRef`-Zeichenkette.
    pub bot_token_ref: SecretRef,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub transport_webhook: Option<TelegramWebhookToml>,
    #[serde(default)]
    pub groups: TelegramGroupsToml,
    #[serde(default)]
    pub topics: TelegramTopicsToml,
    #[serde(default)]
    pub security: TelegramSecurityToml,
    #[serde(default)]
    pub rate_limit: TelegramRateLimitToml,
    #[serde(default)]
    pub attachments: TelegramAttachmentsToml,
    #[serde(default)]
    pub commands: TelegramCommandsToml,
    /// Arbeitsbereiche, die Chats per `/workspace <alias>` wählen können
    /// (`[[channel.telegram.workspaces]]`). Leer = keine Workspace-Auswahl.
    #[serde(default)]
    pub workspaces: Vec<TelegramWorkspaceToml>,
}

/// Ein benannter Arbeitsbereich einer Telegram-Bindung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramWorkspaceToml {
    /// Kurzname, den Chats in `/workspace <alias>` verwenden.
    pub alias: String,
    /// Mandant (`TenantId`), dem der Arbeitsbereich gehört.
    pub tenant: String,
    /// Wurzelverzeichnis des Arbeitsbereichs.
    pub root: String,
    /// Standard-Arbeitsbereich für Chats ohne eigene Auswahl (höchstens einer).
    #[serde(default)]
    pub default: bool,
}

impl TelegramChannelToml {
    /// Prüft die Telegram-spezifischen Zusatzfelder (Befehlsmenü,
    /// Arbeitsbereiche). Gedacht für `validate_telegram_binding` in
    /// `discovery.rs`.
    ///
    /// # Errors
    /// `ConfigError::Invalid` bei unbekannten Werten für `menu_source` bzw.
    /// `unknown_command_fallback`, leeren oder doppelten Workspace-Aliasen,
    /// leeren `tenant`/`root`-Angaben oder mehr als einem `default = true`.
    pub fn validate_extensions(&self) -> Result<(), ConfigError> {
        let channel_id = &self.id;
        self.commands.validate().map_err(|reason| {
            ConfigError::Invalid(format!("Telegram channel {channel_id:?}: {reason}"))
        })?;
        let mut aliases = std::collections::HashSet::new();
        let mut defaults = 0_usize;
        for workspace in &self.workspaces {
            let alias = workspace.alias.trim();
            if alias.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "Telegram channel {channel_id:?}: workspaces.alias darf nicht leer sein"
                )));
            }
            if !aliases.insert(alias.to_owned()) {
                return Err(ConfigError::Invalid(format!(
                    "Telegram channel {channel_id:?}: doppelter workspaces.alias {alias:?}"
                )));
            }
            if workspace.tenant.trim().is_empty() || workspace.root.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "Telegram channel {channel_id:?}: workspace {alias:?} braucht nicht-leere tenant- und root-Angaben"
                )));
            }
            if workspace.default {
                defaults += 1;
            }
        }
        if defaults > 1 {
            return Err(ConfigError::Invalid(format!(
                "Telegram channel {channel_id:?}: höchstens ein Workspace darf default = true setzen"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramWebhookToml {
    pub public_url: String,
    pub secret_token_ref: SecretRef,
    pub listen_addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramGroupsToml {
    /// In Gruppen nur auf Nachrichten reagieren, die den Bot erwähnen
    /// (Standard: `true`).
    #[serde(default = "default_true")]
    pub require_mention: bool,
    #[serde(default)]
    pub observe_unmentioned: bool,
    #[serde(default)]
    pub allowed_chats: Vec<String>,
    #[serde(default)]
    pub group_allowed_senders: Vec<String>,
}

impl Default for TelegramGroupsToml {
    fn default() -> Self {
        Self {
            require_mention: default_true(),
            observe_unmentioned: false,
            allowed_chats: Vec::new(),
            group_allowed_senders: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramTopicsToml {
    #[serde(default = "default_topic_mode")]
    pub mode: String,
}

/// Telegram-specific admission security settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramSecurityToml {
    /// Telegram user or chat IDs allowed through the identity-pinning gate.
    #[serde(default)]
    pub pinned_identities: Vec<i64>,
    /// Erlaubt `/pair <Code>` im Direktchat auch für noch nicht gepinnte
    /// Absender (Standard: `false`, fail-closed).
    #[serde(default)]
    pub allow_unpinned_pairing: bool,
    /// Telegram-User-IDs mit Administratorrechten (z. B. fremde
    /// Arbeitsaufträge genehmigen/ablehnen).
    #[serde(default)]
    pub admin_identities: Vec<i64>,
}

impl Default for TelegramTopicsToml {
    fn default() -> Self {
        Self {
            mode: default_topic_mode(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramRateLimitToml {
    #[serde(default = "default_updates_per_min")]
    pub max_updates_per_peer_per_min: u32,
    #[serde(default = "default_outbound_per_sec")]
    pub max_outbound_per_chat_per_sec: u32,
}

impl Default for TelegramRateLimitToml {
    fn default() -> Self {
        Self {
            max_updates_per_peer_per_min: default_updates_per_min(),
            max_outbound_per_chat_per_sec: default_outbound_per_sec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramAttachmentsToml {
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_max_count")]
    pub max_count_per_message: u32,
    /// Erlaubte MIME-Typen; `type/*` als Platzhalter erlaubt.
    #[serde(default = "default_mime_allowlist")]
    pub mime_allowlist: Vec<String>,
    /// Aufbewahrungsdauer zwischengespeicherter Anhänge in Sekunden
    /// (Standard: 86 400 = 24 h).
    #[serde(default = "default_attachment_expiry_secs")]
    pub expiry_secs: u64,
}

impl Default for TelegramAttachmentsToml {
    fn default() -> Self {
        Self {
            max_bytes: default_max_bytes(),
            max_count_per_message: default_max_count(),
            mime_allowlist: default_mime_allowlist(),
            expiry_secs: default_attachment_expiry_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramCommandsToml {
    /// Herkunft des Befehlsmenüs: `"policy_visible"` (Standard), `"static"`
    /// oder `"none"` (siehe [`TELEGRAM_MENU_SOURCES`]).
    #[serde(default = "default_menu_source")]
    pub menu_source: String,
    /// Verhalten bei unbekannten Befehlen: `"reply_help"` (Standard),
    /// `"pass_through"` oder `"ignore"` (siehe
    /// [`TELEGRAM_UNKNOWN_COMMAND_FALLBACKS`]).
    #[serde(default = "default_unknown_command_fallback")]
    pub unknown_command_fallback: String,
}

/// Zulässige Werte für `[channel.telegram.commands].menu_source`.
pub const TELEGRAM_MENU_SOURCES: &[&str] = &["policy_visible", "static", "none"];

/// Zulässige Werte für `[channel.telegram.commands].unknown_command_fallback`.
pub const TELEGRAM_UNKNOWN_COMMAND_FALLBACKS: &[&str] = &["reply_help", "pass_through", "ignore"];

impl TelegramCommandsToml {
    /// Prüft `menu_source` und `unknown_command_fallback` gegen die
    /// zulässigen Werte.
    ///
    /// # Errors
    /// Beschreibung des ersten unzulässigen Werts.
    pub fn validate(&self) -> Result<(), String> {
        if !TELEGRAM_MENU_SOURCES.contains(&self.menu_source.as_str()) {
            return Err(format!(
                "commands.menu_source {:?} ist unzulässig (erlaubt: policy_visible, static, none)",
                self.menu_source
            ));
        }
        if !TELEGRAM_UNKNOWN_COMMAND_FALLBACKS.contains(&self.unknown_command_fallback.as_str()) {
            return Err(format!(
                "commands.unknown_command_fallback {:?} ist unzulässig (erlaubt: reply_help, pass_through, ignore)",
                self.unknown_command_fallback
            ));
        }
        Ok(())
    }
}

impl Default for TelegramCommandsToml {
    fn default() -> Self {
        Self {
            menu_source: default_menu_source(),
            unknown_command_fallback: default_unknown_command_fallback(),
        }
    }
}

/// Getaggter Channel-Typ nach Discovery-Merge, unabhängig von der
/// konkreten Art (aktuell nur Telegram).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelToml {
    Telegram(TelegramChannelToml),
}

impl ChannelToml {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Telegram(t) => &t.id,
        }
    }
}

/// Liest alle `[[channel.telegram]]`-Einträge einer geparsten Datei aus und
/// gibt sie als benannte `ChannelToml`-Map zurück (Schlüssel = `id`).
#[must_use]
pub fn flatten_channel_file(file: ChannelFileToml) -> HashMap<String, ChannelToml> {
    let mut out = HashMap::new();
    for tg in file.channel.telegram {
        out.insert(tg.id.clone(), ChannelToml::Telegram(tg));
    }
    out
}

fn default_tenant_binding() -> String {
    "pairing".to_owned()
}
fn default_transport() -> String {
    "long_poll".to_owned()
}
fn default_true() -> bool {
    true
}
fn default_topic_mode() -> String {
    "per_topic_session".to_owned()
}
fn default_updates_per_min() -> u32 {
    20
}
fn default_outbound_per_sec() -> u32 {
    1
}
fn default_max_bytes() -> u64 {
    20_000_000
}
fn default_max_count() -> u32 {
    10
}
fn default_mime_allowlist() -> Vec<String> {
    [
        "image/jpeg",
        "image/png",
        "image/webp",
        "application/pdf",
        "text/plain",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
fn default_attachment_expiry_secs() -> u64 {
    86_400
}
fn default_menu_source() -> String {
    "policy_visible".to_owned()
}
fn default_unknown_command_fallback() -> String {
    "reply_help".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_parse_telegram_channel_toml() -> TestResult {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"
        "#;
        let file: ChannelFileToml =
            toml::from_str(src).map_err(ctx("Telegram-Channel-TOML parsen"))?;
        let flattened = flatten_channel_file(file);
        assert!(flattened.contains_key("telegram:support-bot"));
        let telegram = match flattened.get("telegram:support-bot") {
            Some(ChannelToml::Telegram(telegram)) => telegram,
            None => return Err(TestError::Missing("parsed Telegram channel")),
        };
        assert!(telegram.security.pinned_identities.is_empty());
        Ok(())
    }

    #[test]
    fn test_telegram_channel_rejects_plaintext_token() {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "12345:literal-token-value"
        "#;
        let result: Result<ChannelFileToml, _> = toml::from_str(src);
        assert!(result.is_err());
    }

    #[test]
    fn test_telegram_channel_rejects_misspelled_admission_field() {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"

            [channel.telegram.groups]
            require_menton = true
        "#;
        let result: Result<ChannelFileToml, _> = toml::from_str(src);
        assert!(result.is_err());
    }

    #[test]
    fn test_telegram_channel_rejects_misspelled_security_field() {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"

            [channel.telegram.security]
            pined_identities = [123456789]
        "#;
        let result: Result<ChannelFileToml, _> = toml::from_str(src);
        assert!(result.is_err());
    }

    #[test]
    fn test_telegram_channel_parses_pinned_identities() -> TestResult {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"

            [channel.telegram.security]
            pinned_identities = [123456789, -1009876543210]
        "#;
        let file: ChannelFileToml =
            toml::from_str(src).map_err(ctx("Telegram-Channel-TOML parsen"))?;
        let telegram = &file.channel.telegram[0];

        assert_eq!(
            telegram.security.pinned_identities,
            [123456789, -1009876543210]
        );
        Ok(())
    }

    #[test]
    fn test_telegram_defaults_match_serde_defaults() -> TestResult {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"
        "#;
        let file: ChannelFileToml =
            toml::from_str(src).map_err(ctx("Telegram-Channel-TOML parsen"))?;
        let telegram = file
            .channel
            .telegram
            .first()
            .ok_or(TestError::Missing("parsed Telegram channel"))?;

        assert!(telegram.groups.require_mention);
        assert!(TelegramGroupsToml::default().require_mention);
        assert!(!telegram.security.allow_unpinned_pairing);
        assert!(telegram.security.admin_identities.is_empty());
        assert_eq!(telegram.attachments.expiry_secs, 86_400);
        assert_eq!(
            telegram.attachments.mime_allowlist,
            [
                "image/jpeg",
                "image/png",
                "image/webp",
                "application/pdf",
                "text/plain"
            ]
        );
        assert_eq!(
            telegram.attachments.mime_allowlist,
            TelegramAttachmentsToml::default().mime_allowlist
        );
        assert_eq!(
            telegram.attachments.expiry_secs,
            TelegramAttachmentsToml::default().expiry_secs
        );
        assert_eq!(telegram.commands.menu_source, "policy_visible");
        assert_eq!(telegram.commands.unknown_command_fallback, "reply_help");
        assert!(telegram.workspaces.is_empty());
        assert!(telegram.validate_extensions().is_ok());
        Ok(())
    }

    #[test]
    fn test_telegram_parses_workspaces_and_security_extensions() -> TestResult {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"

            [channel.telegram.security]
            pinned_identities = [123456789]
            allow_unpinned_pairing = true
            admin_identities = [123456789]

            [channel.telegram.attachments]
            expiry_secs = 3600
            mime_allowlist = ["image/*"]

            [channel.telegram.commands]
            menu_source = "static"
            unknown_command_fallback = "ignore"

            [[channel.telegram.workspaces]]
            alias = "main"
            tenant = "default"
            root = "/srv/main"
            default = true

            [[channel.telegram.workspaces]]
            alias = "docs"
            tenant = "default"
            root = "/srv/docs"
        "#;
        let file: ChannelFileToml =
            toml::from_str(src).map_err(ctx("Telegram-Channel-TOML parsen"))?;
        let telegram = file
            .channel
            .telegram
            .first()
            .ok_or(TestError::Missing("parsed Telegram channel"))?;

        assert_eq!(
            telegram.workspaces,
            [
                TelegramWorkspaceToml {
                    alias: "main".to_owned(),
                    tenant: "default".to_owned(),
                    root: "/srv/main".to_owned(),
                    default: true,
                },
                TelegramWorkspaceToml {
                    alias: "docs".to_owned(),
                    tenant: "default".to_owned(),
                    root: "/srv/docs".to_owned(),
                    default: false,
                },
            ]
        );
        assert!(telegram.security.allow_unpinned_pairing);
        assert_eq!(telegram.security.admin_identities, [123456789]);
        assert_eq!(telegram.attachments.expiry_secs, 3600);
        assert_eq!(telegram.attachments.mime_allowlist, ["image/*"]);
        assert!(telegram.validate_extensions().is_ok());
        Ok(())
    }

    #[test]
    fn test_telegram_workspace_rejects_unknown_field() {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"

            [[channel.telegram.workspaces]]
            alias = "main"
            tenant = "default"
            root = "/srv/main"
            defualt = true
        "#;
        let result: Result<ChannelFileToml, _> = toml::from_str(src);
        assert!(result.is_err());
    }

    #[test]
    fn test_telegram_validate_extensions_rejects_bad_values() -> TestResult {
        let base = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"
        "#;
        let cases = [
            "[channel.telegram.commands]\nmenu_source = \"everything\"\n",
            "[channel.telegram.commands]\nunknown_command_fallback = \"shout\"\n",
            "[[channel.telegram.workspaces]]\nalias = \"a\"\ntenant = \"t\"\nroot = \"/a\"\ndefault = true\n[[channel.telegram.workspaces]]\nalias = \"b\"\ntenant = \"t\"\nroot = \"/b\"\ndefault = true\n",
            "[[channel.telegram.workspaces]]\nalias = \"a\"\ntenant = \"t\"\nroot = \"/a\"\n[[channel.telegram.workspaces]]\nalias = \"a\"\ntenant = \"t\"\nroot = \"/b\"\n",
            "[[channel.telegram.workspaces]]\nalias = \" \"\ntenant = \"t\"\nroot = \"/a\"\n",
        ];
        for extra in cases {
            let src = format!("{base}\n{extra}");
            let file: ChannelFileToml =
                toml::from_str(&src).map_err(ctx("Telegram-Channel-TOML parsen"))?;
            let telegram = file
                .channel
                .telegram
                .first()
                .ok_or(TestError::Missing("parsed Telegram channel"))?;
            assert!(telegram.validate_extensions().is_err(), "{extra}");
        }
        Ok(())
    }
}
