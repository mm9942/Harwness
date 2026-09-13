use std::fmt;
use std::sync::Arc;

use harw_browser::ids::BrowserContextId;
use harw_browser::page_bridge::PageBridgeInstallRequest;
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

const ENVELOPE_SCHEMA: &str = "harwness.browser-bridge-message/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgePolicy {
    max_payload_bytes: usize,
    max_messages: u32,
    window: Duration,
}

impl BridgePolicy {
    pub fn new(
        max_payload_bytes: usize,
        max_messages: u32,
        window: Duration,
    ) -> Result<Self, BridgeConfigError> {
        if max_payload_bytes == 0 {
            return Err(BridgeConfigError::ZeroPayloadLimit);
        }
        if max_messages == 0 {
            return Err(BridgeConfigError::ZeroRateLimit);
        }
        if !window.is_positive() {
            return Err(BridgeConfigError::NonPositiveRateWindow);
        }
        Ok(Self {
            max_payload_bytes,
            max_messages,
            window,
        })
    }

    pub const fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }
    pub const fn max_messages(&self) -> u32 {
        self.max_messages
    }
    pub const fn window(&self) -> Duration {
        self.window
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeProvenance {
    bridge_id: String,
    version: String,
    script_sha256: String,
}

impl BridgeProvenance {
    pub fn bridge_id(&self) -> &str {
        &self.bridge_id
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn script_sha256(&self) -> &str {
        &self.script_sha256
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct BridgeDefinition {
    script: String,
    policy: BridgePolicy,
    provenance: Arc<BridgeProvenance>,
}

impl BridgeDefinition {
    pub fn new(
        bridge_id: impl Into<String>,
        version: impl Into<String>,
        script: impl Into<String>,
        policy: BridgePolicy,
    ) -> Result<Self, BridgeConfigError> {
        let bridge_id = bridge_id.into();
        let version = version.into();
        let script = script.into();
        if bridge_id.trim().is_empty() {
            return Err(BridgeConfigError::EmptyBridgeId);
        }
        if version.trim().is_empty() {
            return Err(BridgeConfigError::EmptyVersion);
        }
        if script.trim().is_empty() {
            return Err(BridgeConfigError::EmptyScript);
        }
        let provenance = Arc::new(BridgeProvenance {
            bridge_id,
            version,
            script_sha256: sha256_hex(script.as_bytes()),
        });
        Ok(Self {
            script,
            policy,
            provenance,
        })
    }

    pub fn script(&self) -> &str {
        &self.script
    }
    pub const fn policy(&self) -> &BridgePolicy {
        &self.policy
    }
    pub fn provenance(&self) -> &BridgeProvenance {
        &self.provenance
    }

    pub fn to_install_request(
        &self,
        context_id: BrowserContextId,
    ) -> Result<PageBridgeInstallRequest, BridgeConfigError> {
        let window = std::time::Duration::try_from(self.policy.window)
            .map_err(|_| BridgeConfigError::RateWindowOutOfRange)?;
        let policy = harw_browser::page_bridge::PageBridgePolicy::new(
            self.policy.max_payload_bytes,
            self.policy.max_messages,
            window,
        )
        .map_err(|_| BridgeConfigError::CoreRequestRejected)?;
        PageBridgeInstallRequest::new(
            context_id,
            self.provenance.bridge_id(),
            self.provenance.version(),
            self.script(),
            policy,
        )
        .map_err(|_| BridgeConfigError::CoreRequestRejected)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeConfigError {
    EmptyBridgeId,
    EmptyVersion,
    EmptyScript,
    ZeroPayloadLimit,
    ZeroRateLimit,
    NonPositiveRateWindow,
    RateWindowOutOfRange,
    CoreRequestRejected,
}

impl fmt::Display for BridgeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyBridgeId => "page bridge ID must not be empty",
            Self::EmptyVersion => "page bridge version must not be empty",
            Self::EmptyScript => "page bridge script must not be empty",
            Self::ZeroPayloadLimit => "page bridge payload limit must be greater than zero",
            Self::ZeroRateLimit => "page bridge rate limit must be greater than zero",
            Self::NonPositiveRateWindow => "page bridge rate window must be positive",
            Self::RateWindowOutOfRange => {
                "page bridge rate window cannot be represented by std::time::Duration"
            }
            Self::CoreRequestRejected => {
                "core page bridge request rejected a previously validated bridge definition"
            }
        })
    }
}

impl std::error::Error for BridgeConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentTrust {
    Untrusted,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BridgeMessage {
    channel_id: String,
    conversation_id: String,
    external_message_id: Option<String>,
    sender: String,
    timestamp: String,
    text: String,
    provenance: Arc<BridgeProvenance>,
}

impl BridgeMessage {
    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    pub fn external_message_id(&self) -> Option<&str> {
        self.external_message_id.as_deref()
    }
    pub fn sender(&self) -> &str {
        &self.sender
    }
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub const fn content_trust(&self) -> ContentTrust {
        ContentTrust::Untrusted
    }
    pub fn bridge_provenance(&self) -> &BridgeProvenance {
        &self.provenance
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BridgeIngestOutcome {
    Accepted(BridgeMessage),
    Rejected(BridgeRejection),
}

#[derive(Debug, PartialEq, Eq)]
pub enum BridgeRejection {
    PayloadTooLarge {
        max_bytes: usize,
        actual_bytes: usize,
    },
    InvalidEnvelope,
    UnsupportedEnvelopeSchema {
        found: String,
    },
    RateLimited {
        retry_after: Duration,
    },
}

pub struct PageBridgeIngestor {
    definition: BridgeDefinition,
    rate_window: Option<RateWindow>,
}

struct RateWindow {
    started_at: OffsetDateTime,
    accepted: u32,
}

impl PageBridgeIngestor {
    pub fn new(definition: BridgeDefinition) -> Self {
        Self {
            definition,
            rate_window: None,
        }
    }

    pub fn definition(&self) -> &BridgeDefinition {
        &self.definition
    }

    pub fn ingest(&mut self, received_at: OffsetDateTime, payload: &[u8]) -> BridgeIngestOutcome {
        let policy = self.definition.policy;
        if payload.len() > policy.max_payload_bytes {
            return BridgeIngestOutcome::Rejected(BridgeRejection::PayloadTooLarge {
                max_bytes: policy.max_payload_bytes,
                actual_bytes: payload.len(),
            });
        }

        let envelope: Envelope = match serde_json::from_slice(payload) {
            Ok(envelope) => envelope,
            Err(_) => return BridgeIngestOutcome::Rejected(BridgeRejection::InvalidEnvelope),
        };
        if envelope.schema != ENVELOPE_SCHEMA {
            return BridgeIngestOutcome::Rejected(BridgeRejection::UnsupportedEnvelopeSchema {
                found: envelope.schema,
            });
        }
        if !envelope.is_valid() {
            return BridgeIngestOutcome::Rejected(BridgeRejection::InvalidEnvelope);
        }

        if let Some(retry_after) = self.rate_limit(received_at) {
            return BridgeIngestOutcome::Rejected(BridgeRejection::RateLimited { retry_after });
        }

        BridgeIngestOutcome::Accepted(BridgeMessage {
            channel_id: envelope.channel_id,
            conversation_id: envelope.conversation_id,
            external_message_id: envelope.external_message_id,
            sender: envelope.sender,
            timestamp: envelope.timestamp,
            text: envelope.content.into_text(),
            provenance: Arc::clone(&self.definition.provenance),
        })
    }

    fn rate_limit(&mut self, received_at: OffsetDateTime) -> Option<Duration> {
        let policy = self.definition.policy;
        let window = self.rate_window.get_or_insert(RateWindow {
            started_at: received_at,
            accepted: 0,
        });
        let elapsed = received_at - window.started_at;
        if elapsed >= policy.window {
            window.started_at = received_at;
            window.accepted = 0;
        }
        if window.accepted >= policy.max_messages {
            let bounded_elapsed = (received_at - window.started_at).max(Duration::ZERO);
            return Some((policy.window - bounded_elapsed).max(Duration::ZERO));
        }
        window.accepted += 1;
        None
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: String,
    channel_id: String,
    conversation_id: String,
    external_message_id: Option<String>,
    sender: String,
    timestamp: String,
    content: EnvelopeContent,
}

impl Envelope {
    fn is_valid(&self) -> bool {
        !self.channel_id.trim().is_empty()
            && !self.conversation_id.trim().is_empty()
            && self
                .external_message_id
                .as_deref()
                .is_none_or(|id| !id.trim().is_empty())
            && !self.sender.trim().is_empty()
            && !self.timestamp.trim().is_empty()
            && !self.content.text().is_empty()
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum EnvelopeContent {
    Text { text: String },
}

impl EnvelopeContent {
    fn text(&self) -> &str {
        match self {
            Self::Text { text } => text,
        }
    }
}

impl EnvelopeContent {
    fn into_text(self) -> String {
        match self {
            Self::Text { text } => text,
        }
    }
}

const SHA256_INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_hex(input: &[u8]) -> String {
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = SHA256_INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}
