//! Channel-Binding-Konfiguration (`channels/*.toml`).
//!
//! Telegram ist die erste konkrete Bindung (siehe
//! `docs/design/channel-ingress-telegram.md` §3.1). Weitere Channel-Arten
//! (Slack, E-Mail, ...) bekommen eigene `Vec<...>`-Felder in
//! `ChannelSectionToml`, ohne das bestehende Schema zu brechen.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auth_toml::SecretRef;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramWebhookToml {
    pub public_url: String,
    pub secret_token_ref: SecretRef,
    pub listen_addr: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramGroupsToml {
    #[serde(default)]
    pub require_mention: bool,
    #[serde(default)]
    pub observe_unmentioned: bool,
    #[serde(default)]
    pub allowed_chats: Vec<String>,
    #[serde(default)]
    pub group_allowed_senders: Vec<String>,
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
    #[serde(default)]
    pub mime_allowlist: Vec<String>,
}

impl Default for TelegramAttachmentsToml {
    fn default() -> Self {
        Self {
            max_bytes: default_max_bytes(),
            max_count_per_message: default_max_count(),
            mime_allowlist: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramCommandsToml {
    #[serde(default = "default_menu_source")]
    pub menu_source: String,
    #[serde(default = "default_unknown_command_fallback")]
    pub unknown_command_fallback: String,
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
fn default_menu_source() -> String {
    "policy_visible".to_owned()
}
fn default_unknown_command_fallback() -> String {
    "reply_help".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_telegram_channel_toml() {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"
        "#;
        let file: ChannelFileToml = toml::from_str(src).unwrap();
        let flattened = flatten_channel_file(file);
        assert!(flattened.contains_key("telegram:support-bot"));
        let telegram = match flattened.get("telegram:support-bot") {
            Some(ChannelToml::Telegram(telegram)) => telegram,
            None => panic!("parsed Telegram channel is missing"),
        };
        assert!(telegram.security.pinned_identities.is_empty());
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
    fn test_telegram_channel_parses_pinned_identities() {
        let src = r#"
            [[channel.telegram]]
            id = "telegram:support-bot"
            bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"

            [channel.telegram.security]
            pinned_identities = [123456789, -1009876543210]
        "#;
        let file: ChannelFileToml = toml::from_str(src).unwrap();
        let telegram = &file.channel.telegram[0];

        assert_eq!(
            telegram.security.pinned_identities,
            [123456789, -1009876543210]
        );
    }
}
