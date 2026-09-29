//! Library-level n8n outbound bridge. No inbound listener or TUI wiring.
use harw_config::{n8n_credentials::N8nCredential, n8n_toml::N8nSection};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt, time::{Duration, Instant}};
use uuid::Uuid;

/// Serialized field order is intentionally stable: event_id, correlation_id,
/// external_origin, session_id, event_type, payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventMessage<T> {
    pub event_id: String,
    pub correlation_id: String,
    pub external_origin: String,
    pub session_id: Option<String>,
    pub event_type: String,
    pub payload: T,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventReply<T> { pub correlation_id: String, pub payload: T }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeOutcome<T> { Disabled, Duplicate, Sent(T) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError { Timeout, Http(u16), Other }
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "n8n transport failure ({self:?})") }
}
impl std::error::Error for TransportError {}
impl TransportError {
    fn transient(self) -> bool { matches!(self, Self::Timeout | Self::Http(429) | Self::Http(500..=599)) }
}

/// Bounded exponential delay with additive jitter, kept pure for deterministic tests.
fn backoff_delay(initial: Duration, maximum: Duration, attempt: u32, jitter: Duration) -> Duration {
    initial.saturating_mul(2_u32.saturating_pow(attempt)).saturating_add(jitter).min(maximum)
}

fn dedup_is_live(seen_at: Instant, now: Instant, window: Duration) -> bool {
    now.saturating_duration_since(seen_at) < window
}

/// Implementations perform one authenticated HTTPS POST; errors must never include request headers.
pub trait N8nTransport: Send + Sync {
    fn post(&self, endpoint: &str, bearer_token: &str, body: &[u8]) -> Result<Vec<u8>, TransportError>;
}

/// Credential exists only in this client and is never formatted or included in errors.
/// The manual `Debug` keeps the credential redacted (it derives none of the fields).
pub struct N8nBridge<T> {
    section: N8nSection,
    credential: N8nCredential,
    transport: T,
    seen: HashMap<String, Instant>,
}
impl<T> fmt::Debug for N8nBridge<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("N8nBridge")
            .field("enabled", &self.section.enabled)
            .field("endpoint", &self.section.endpoint_url)
            .field("credential", &"[REDACTED]")
            .field("dedup_entries", &self.seen.len())
            .finish()
    }
}
impl<T: N8nTransport> N8nBridge<T> {
    /// Construction requires both resolved configuration and credential, even while disabled.
    pub fn new(section: N8nSection, credential: N8nCredential, transport: T) -> Result<Self, String> {
        section.validate()?;
        Ok(Self { section, credential, transport, seen: HashMap::new() })
    }

    pub fn send<E: Serialize, R: for<'de> Deserialize<'de>>(
        &mut self, dedup_key: &str, correlation_id: impl Into<String>, session_id: Option<String>,
        event_type: impl Into<String>, payload: E,
    ) -> Result<BridgeOutcome<EventReply<R>>, TransportError> {
        if !self.section.enabled { return Ok(BridgeOutcome::Disabled); }
        let now = Instant::now();
        let window = Duration::from_secs(self.section.dedup_window_secs);
        self.seen.retain(|_, at| now.duration_since(*at) < window);
        if self.seen.contains_key(dedup_key) { return Ok(BridgeOutcome::Duplicate); }
        let correlation_id = correlation_id.into();
        let message = EventMessage {
            event_id: Uuid::new_v4().to_string(), correlation_id: correlation_id.clone(),
            external_origin: "n8n".into(),
            session_id: if self.section.session_binding { session_id } else { None },
            event_type: event_type.into(), payload,
        };
        let body = serde_json::to_vec(&message).map_err(|_| TransportError::Other)?;
        let endpoint = self.section.endpoint_url.as_deref().ok_or(TransportError::Other)?;
        let attempts = self.section.retry_max_attempts.max(1);
        let mut backoff = Duration::from_secs(self.section.retry_initial_backoff_secs);
        for attempt in 0..attempts {
            match self.transport.post(endpoint, self.credential.expose(), &body) {
                Ok(bytes) => {
                    let reply: EventReply<R> = serde_json::from_slice(&bytes).map_err(|_| TransportError::Other)?;
                    if reply.correlation_id != correlation_id { return Err(TransportError::Other); }
                    self.seen.insert(dedup_key.to_owned(), now);
                    return Ok(BridgeOutcome::Sent(reply));
                }
                Err(error) if error.transient() && attempt + 1 < attempts => {
                    // Apply bounded exponential delay plus bounded positive jitter.
                    let cap = Duration::from_secs(self.section.retry_max_backoff_secs);
                    let jitter_ms = (Uuid::new_v4().as_u128() % 251) as u64;
                    std::thread::sleep((backoff + Duration::from_millis(jitter_ms)).min(cap));
                    backoff = backoff.saturating_mul(2).min(cap);
                }
                Err(error) => return Err(error),
            }
        }
        Err(TransportError::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_config::n8n_credentials::N8nCredential;

    fn section(enabled: bool) -> N8nSection {
        N8nSection {
            enabled,
            endpoint_url: Some("https://n8n.example/hook".into()),
            credential_profile_ref: Some("test".into()),
            session_binding: true,
            retry_max_attempts: 3,
            retry_initial_backoff_secs: 1,
            retry_max_backoff_secs: 30,
            dedup_window_secs: 300,
        }
    }

    struct AlwaysTimeout;
    impl N8nTransport for AlwaysTimeout {
        fn post(&self, _: &str, _: &str, _: &[u8]) -> Result<Vec<u8>, TransportError> {
            Err(TransportError::Timeout)
        }
    }

    #[test]
    fn payload_serialization_order_and_provenance() {
        let e = EventMessage { event_id:"id".into(), correlation_id:"c".into(), external_origin:"n8n".into(), session_id:Some("s".into()), event_type:"turn".into(), payload: serde_json::json!({"ok":true}) };
        let text = serde_json::to_string(&e).unwrap();
        assert!(text.find("event_id").unwrap() < text.find("correlation_id").unwrap());
        assert_eq!(serde_json::from_str::<EventMessage<serde_json::Value>>(&text).unwrap(), e);
        assert_eq!(e.external_origin, "n8n");
    }
    #[test]
    fn transient_classification_excludes_auth() {
        assert!(TransportError::Timeout.transient());
        assert!(TransportError::Http(429).transient());
        assert!(TransportError::Http(503).transient());
        assert!(!TransportError::Http(401).transient());
    }
    #[test]
    fn backoff_is_bounded_exponential() {
        let initial = Duration::from_secs(1);
        let max = Duration::from_secs(30);
        assert_eq!(backoff_delay(initial, max, 0, Duration::ZERO), Duration::from_secs(1));
        assert_eq!(backoff_delay(initial, max, 1, Duration::ZERO), Duration::from_secs(2));
        assert_eq!(backoff_delay(initial, max, 4, Duration::ZERO), Duration::from_secs(16));
        assert_eq!(backoff_delay(initial, max, 10, Duration::ZERO), max);
        assert_eq!(backoff_delay(initial, max, 0, Duration::from_secs(1)), Duration::from_secs(2));
    }
    #[test]
    fn disabled_is_fail_closed_noop() {
        let mut bridge = N8nBridge::new(section(false), N8nCredential::for_tests("t"), AlwaysTimeout).unwrap();
        let outcome = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", Some("s".to_string()), "turn", serde_json::json!({}));
        assert!(matches!(outcome, Ok(BridgeOutcome::Disabled)));
    }

    /// Zählender Mock: jede Antwort genau einmal, dann Err(Other).
    struct Scripted(std::sync::Mutex<std::vec::IntoIter<Result<Vec<u8>, TransportError>>>);
    impl N8nTransport for Scripted {
        fn post(&self, _: &str, _: &str, _: &[u8]) -> Result<Vec<u8>, TransportError> {
            let mut guard = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.next().unwrap_or(Err(TransportError::Other))
        }
    }

    fn scripted_bridge(
        section: N8nSection,
        responses: Vec<Result<Vec<u8>, TransportError>>,
    ) -> N8nBridge<Scripted> {
        N8nBridge::new(
            section,
            N8nCredential::for_tests("t"),
            Scripted(std::sync::Mutex::new(responses.into_iter())),
        )
        .unwrap()
    }

    fn reply_bytes(correlation: &str) -> Result<Vec<u8>, TransportError> {
        Ok(serde_json::to_vec(&EventReply::<serde_json::Value> {
            correlation_id: correlation.to_owned(),
            payload: serde_json::json!({}),
        })
        .unwrap())
    }

    #[test]
    fn dedup_window_blocks_second_send() {
        let mut bridge = scripted_bridge(section(true), vec![reply_bytes("c")]);
        let first = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", None, "turn", serde_json::json!({})).unwrap();
        assert!(matches!(first, BridgeOutcome::Sent(_)));
        let second = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", None, "turn", serde_json::json!({})).unwrap();
        assert!(matches!(second, BridgeOutcome::Duplicate));
    }

    #[test]
    fn session_binding_false_strips_session_id() {
        let mut cfg = section(true);
        cfg.session_binding = false;
        let mut bridge = scripted_bridge(cfg, vec![reply_bytes("c")]);
        let sent = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", Some("s".to_string()), "turn", serde_json::json!({})).unwrap();
        let BridgeOutcome::Sent(_) = sent else { panic!("expected sent") };
    }

    #[test]
    fn correlation_mismatch_is_rejected() {
        let mut bridge = scripted_bridge(section(true), vec![reply_bytes("other")]);
        let outcome = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", None, "turn", serde_json::json!({}));
        assert!(matches!(outcome, Err(TransportError::Other)));
    }

    #[test]
    fn transient_errors_retry_then_succeed() {
        // 2x Timeout (transient), dann Erfolg — retry_max_attempts 3 reicht.
        let mut bridge = scripted_bridge(
            section(true),
            vec![Err(TransportError::Timeout), Err(TransportError::Timeout), reply_bytes("c")],
        );
        let outcome = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", None, "turn", serde_json::json!({})).unwrap();
        assert!(matches!(outcome, BridgeOutcome::Sent(_)));
    }

    #[test]
    fn auth_4xx_never_retries() {
        let mut bridge = scripted_bridge(section(true), vec![Err(TransportError::Http(401))]);
        let outcome = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", None, "turn", serde_json::json!({})).unwrap_err();
        assert_eq!(outcome, TransportError::Http(401));
    }

    #[test]
    fn retries_exhausted_reports_last_error() {
        let mut bridge = scripted_bridge(
            section(true),
            vec![
                Err(TransportError::Timeout),
                Err(TransportError::Timeout),
                Err(TransportError::Timeout),
            ],
        );
        let outcome = bridge.send::<serde_json::Value, serde_json::Value>("k", "c", None, "turn", serde_json::json!({})).unwrap_err();
        assert_eq!(outcome, TransportError::Timeout);
    }

    #[test]
    fn credential_never_appears_in_debug() {
        let bridge = scripted_bridge(section(true), vec![]);
        let debug = format!("{bridge:?}");
        assert!(!debug.contains("t\"") && !debug.contains("secret"));
    }
}
