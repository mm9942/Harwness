use std::collections::HashSet;

use harw_types::{ChannelId, PeerId};

/// Default inbound admission ceiling (matches
/// `harw_config::channel_toml::TelegramRateLimitToml`'s
/// `max_updates_per_peer_per_min` default) applied whenever a binding is
/// constructed without an explicit rate-limit override, e.g. in tests.
pub const DEFAULT_MAX_UPDATES_PER_PEER_PER_MIN: u32 = 20;

/// Standard-Obergrenze je Anhang in Bytes (Telegram-Bot-API-Download-Limit).
pub const DEFAULT_ATTACHMENT_MAX_BYTES: u64 = 20_000_000;

/// Standard-Höchstzahl an Anhängen je eingehender Nachricht.
pub const DEFAULT_ATTACHMENT_MAX_COUNT: usize = 10;

/// Standard-Liste erlaubter Anhangsarten (MIME, `type/*` als Platzhalter).
fn default_attachment_allowed_kinds() -> Vec<String> {
    vec![
        "image/*".to_owned(),
        "application/pdf".to_owned(),
        "text/plain".to_owned(),
    ]
}

/// Whether Telegram forum topics produce separate harness sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicMode {
    /// Preserve a supplied `message_thread_id` in the structural session key.
    PerTopicSession,
    /// Collapse all topics in a chat into one session.
    SharedSession,
}

/// Load-time policy used by one Telegram bot binding.
///
/// Tokens are deliberately absent: configuration contains secret references and
/// a transport client resolves those outside this pure admission boundary.
///
/// Tenant bindings are no longer stored here; they are resolved durably via
/// [`harw_channel::PairingStore`] at admission time.
#[derive(Debug, Clone)]
pub struct TelegramChannelConfig {
    pub channel_id: ChannelId,
    /// Telegram numeric sender IDs explicitly pinned to this bot binding.
    pub pinned_sender_ids: HashSet<String>,
    pub allowed_group_chats: HashSet<String>,
    pub group_allowed_senders: HashSet<String>,
    pub require_mention_in_groups: bool,
    pub topic_mode: TopicMode,
    /// Inbound admission ceiling per rate-limit key (§3.5), sourced from
    /// `[channel.telegram.rate_limit].max_updates_per_peer_per_min`. `0`
    /// admits nothing for that window (fail closed on a misconfigured `0`,
    /// rather than silently disabling the gate).
    pub max_updates_per_peer_per_min: u32,
    /// Erlaubt ungepinnten Absendern ausschließlich das Einlösen eines
    /// `/pair <Code>` im privaten DM (`[channel.telegram.security]
    /// allow_unpinned_pairing`). Standard `false`: ungepinnter Verkehr wird
    /// vor jeder dauerhaften Schreiboperation verworfen.
    pub allow_unpinned_pairing: bool,
    /// Telegram-User-IDs mit Administratorrechten für Work-Requests
    /// (`[channel.telegram.security] admin_identities`).
    pub admin_sender_ids: HashSet<String>,
    /// Obergrenze je Anhang in Bytes.
    pub attachment_max_bytes: u64,
    /// Höchstzahl an Anhängen je Nachricht.
    pub attachment_max_count: usize,
    /// Erlaubte Anhangsarten (MIME, `type/*` als Platzhalter).
    pub attachment_allowed_kinds: Vec<String>,
}

impl TelegramChannelConfig {
    #[must_use]
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            pinned_sender_ids: HashSet::new(),
            allowed_group_chats: HashSet::new(),
            group_allowed_senders: HashSet::new(),
            require_mention_in_groups: true,
            topic_mode: TopicMode::PerTopicSession,
            max_updates_per_peer_per_min: DEFAULT_MAX_UPDATES_PER_PEER_PER_MIN,
            allow_unpinned_pairing: false,
            admin_sender_ids: HashSet::new(),
            attachment_max_bytes: DEFAULT_ATTACHMENT_MAX_BYTES,
            attachment_max_count: DEFAULT_ATTACHMENT_MAX_COUNT,
            attachment_allowed_kinds: default_attachment_allowed_kinds(),
        }
    }

    #[must_use]
    pub fn is_group(&self, peer: &PeerId) -> bool {
        self.allowed_group_chats.contains(peer.as_str())
    }

    /// Returns whether a Telegram sender ID is explicitly pinned to this binding.
    #[must_use]
    pub fn is_sender_identity_pinned(&self, sender_id: &str) -> bool {
        self.pinned_sender_ids.contains(sender_id)
    }

    /// Ob eine Telegram-User-ID als Administrator konfiguriert ist.
    #[must_use]
    pub fn is_admin_sender(&self, id: &str) -> bool {
        self.admin_sender_ids.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::TelegramChannelConfig;
    use harw_types::ChannelId;

    fn config() -> TelegramChannelConfig {
        TelegramChannelConfig::new(ChannelId::from_str("telegram:ops"))
    }

    #[test]
    fn default_config_has_no_pinned_sender_identities() {
        let config = config();

        assert!(config.pinned_sender_ids.is_empty());
        assert!(!config.is_sender_identity_pinned("123456789"));
    }

    #[test]
    fn pinned_sender_identity_is_allowed() {
        let mut config = config();
        config.pinned_sender_ids.insert("123456789".to_owned());

        assert!(config.is_sender_identity_pinned("123456789"));
    }

    #[test]
    fn unpinned_sender_identity_is_denied() {
        let mut config = config();
        config.pinned_sender_ids.insert("123456789".to_owned());

        assert!(!config.is_sender_identity_pinned("987654321"));
    }

    #[test]
    fn default_config_uses_the_documented_rate_limit_default() {
        assert_eq!(
            config().max_updates_per_peer_per_min,
            super::DEFAULT_MAX_UPDATES_PER_PEER_PER_MIN
        );
        assert_eq!(super::DEFAULT_MAX_UPDATES_PER_PEER_PER_MIN, 20);
    }

    #[test]
    fn default_config_is_fail_closed_for_pairing_and_admins() {
        let config = config();

        assert!(!config.allow_unpinned_pairing);
        assert!(config.admin_sender_ids.is_empty());
        assert!(!config.is_admin_sender("123456789"));
    }

    #[test]
    fn configured_admin_sender_is_recognized() {
        let mut config = config();
        config.admin_sender_ids.insert("123456789".to_owned());

        assert!(config.is_admin_sender("123456789"));
        assert!(!config.is_admin_sender("987654321"));
    }

    #[test]
    fn default_attachment_limits_match_documented_defaults() {
        let config = config();

        assert_eq!(config.attachment_max_bytes, 20_000_000);
        assert_eq!(config.attachment_max_count, 10);
        assert_eq!(
            config.attachment_allowed_kinds,
            vec![
                "image/*".to_owned(),
                "application/pdf".to_owned(),
                "text/plain".to_owned(),
            ]
        );
    }
}
