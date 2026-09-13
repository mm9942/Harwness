use time::{Duration, OffsetDateTime};

/// Content received from an external channel.
///
/// Values of this type are untrusted user data. They carry no instruction or
/// authority semantics.
#[derive(Debug, PartialEq, Eq)]
pub enum MessageContent {
    Text(String),
}

/// Identity used to deduplicate normalized messages.
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum MessageIdentity {
    /// Stable identity supplied by the external application.
    External(String),
    /// Locally derived identity used when the application supplies no ID.
    Fingerprint(String),
}

/// Connector-owned extraction result before normalization.
#[derive(Debug, PartialEq, Eq)]
pub struct RawChannelMessage {
    channel_id: String,
    conversation_id: String,
    sender: String,
    timestamp: OffsetDateTime,
    content: MessageContent,
    external_message_id: Option<String>,
}

impl RawChannelMessage {
    #[must_use]
    pub fn new(
        channel_id: impl Into<String>,
        conversation_id: impl Into<String>,
        sender: impl Into<String>,
        timestamp: OffsetDateTime,
        content: MessageContent,
    ) -> Self {
        Self {
            channel_id: channel_id.into(),
            conversation_id: conversation_id.into(),
            sender: sender.into(),
            timestamp,
            content,
            external_message_id: None,
        }
    }

    #[must_use]
    pub fn with_external_message_id(mut self, external_message_id: impl Into<String>) -> Self {
        self.external_message_id = Some(external_message_id.into());
        self
    }
}

/// Canonical message entering the channel perimeter.
#[derive(Debug, PartialEq, Eq)]
pub struct ChannelMessage {
    channel_id: String,
    conversation_id: String,
    sender: String,
    timestamp: OffsetDateTime,
    content: MessageContent,
    identity: MessageIdentity,
    connector_id: String,
    identity_is_uncertain: bool,
}

impl ChannelMessage {
    #[must_use]
    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    #[must_use]
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    #[must_use]
    pub fn sender(&self) -> &str {
        &self.sender
    }

    #[must_use]
    pub fn timestamp(&self) -> OffsetDateTime {
        self.timestamp
    }

    #[must_use]
    pub fn content(&self) -> &MessageContent {
        &self.content
    }

    #[must_use]
    pub fn identity(&self) -> &MessageIdentity {
        &self.identity
    }

    #[must_use]
    pub fn connector_id(&self) -> &str {
        &self.connector_id
    }

    #[must_use]
    pub fn identity_is_uncertain(&self) -> bool {
        self.identity_is_uncertain
    }
}

/// Converts extracted messages to their canonical representation and dedupe
/// identity.
#[derive(Debug, PartialEq, Eq)]
pub struct MessageNormalizer {
    connector_id: String,
    timestamp_window_seconds: i64,
}

impl MessageNormalizer {
    const DEFAULT_TIMESTAMP_WINDOW: Duration = Duration::minutes(1);

    #[must_use]
    pub fn new(connector_id: impl Into<String>) -> Self {
        Self::with_timestamp_window(connector_id, Self::DEFAULT_TIMESTAMP_WINDOW)
    }

    #[must_use]
    pub fn with_timestamp_window(
        connector_id: impl Into<String>,
        timestamp_window: Duration,
    ) -> Self {
        Self {
            connector_id: connector_id.into(),
            timestamp_window_seconds: timestamp_window.whole_seconds().max(1),
        }
    }

    #[must_use]
    pub fn normalize(&self, raw: RawChannelMessage) -> ChannelMessage {
        let RawChannelMessage {
            channel_id,
            conversation_id,
            sender,
            timestamp,
            content,
            external_message_id,
        } = raw;

        let (identity, identity_is_uncertain) = match external_message_id {
            Some(external_message_id) => (MessageIdentity::External(external_message_id), false),
            None => {
                let fingerprint = message_fingerprint(
                    &channel_id,
                    &conversation_id,
                    &sender,
                    timestamp,
                    &content,
                    self.timestamp_window_seconds,
                );
                (MessageIdentity::Fingerprint(fingerprint), true)
            }
        };

        ChannelMessage {
            channel_id,
            conversation_id,
            sender,
            timestamp,
            content,
            identity,
            connector_id: self.connector_id.to_owned(),
            identity_is_uncertain,
        }
    }
}

fn message_fingerprint(
    channel_id: &str,
    conversation_id: &str,
    sender: &str,
    timestamp: OffsetDateTime,
    content: &MessageContent,
    timestamp_window_seconds: i64,
) -> String {
    let timestamp_bucket = timestamp
        .unix_timestamp()
        .div_euclid(timestamp_window_seconds);
    let mut first = StableHasher::new(0xcbf2_9ce4_8422_2325);
    let mut second = StableHasher::new(0x8422_2325_cbf2_9ce4);

    for hasher in [&mut first, &mut second] {
        hasher.write_field(channel_id.as_bytes());
        hasher.write_field(conversation_id.as_bytes());
        hasher.write_field(sender.as_bytes());
        hasher.write_field(&timestamp_bucket.to_be_bytes());
        match content {
            MessageContent::Text(text) => {
                hasher.write_field(b"text");
                hasher.write_field(text.as_bytes());
            }
        }
    }

    format!("{:016x}{:016x}", first.finish(), second.finish())
}

/// A fixed, process-independent FNV-1a variant used only for dedupe keys.
/// Length framing prevents different field boundaries from hashing the same
/// concatenated byte stream.
struct StableHasher {
    state: u64,
}

impl StableHasher {
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn write_field(&mut self, bytes: &[u8]) {
        self.write(&bytes.len().to_be_bytes());
        self.write(bytes);
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
    }

    const fn finish(&self) -> u64 {
        self.state
    }
}
