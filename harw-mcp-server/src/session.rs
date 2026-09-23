use std::collections::HashMap;
use std::fmt;

use jiff::{SignedDuration, Timestamp};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub type McpServerResult<T> = Result<T, McpServerError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerError {
    SessionUnknown,
    SessionNotInitialized,
    SessionPrincipalMismatch,
    ProtocolMismatch { expected: String, actual: String },
    SessionCapacityExceeded,
    InvalidSessionId,
}

impl fmt::Display for McpServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionUnknown => f.write_str("MCP session was not found or has expired"),
            Self::SessionNotInitialized => {
                f.write_str("MCP session has not completed initialization")
            }
            Self::SessionPrincipalMismatch => {
                f.write_str("MCP session belongs to another principal")
            }
            Self::ProtocolMismatch { expected, actual } => write!(
                f,
                "MCP protocol mismatch: expected {expected}, got {actual}"
            ),
            Self::SessionCapacityExceeded => f.write_str("MCP session capacity is exhausted"),
            Self::InvalidSessionId => f.write_str("invalid MCP session id"),
        }
    }
}

impl std::error::Error for McpServerError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSession {
    pub id: String,
    pub protocol_version: String,
    pub initialized_at: Timestamp,
    pub expires_at: Timestamp,
    pub client_name: String,
    pub principal_key: String,
    pub initialized: bool,
}

/// In-memory authority cache. A session is a transport credential, not a
/// durable job credential: jobs persist their own fenced lease token.
pub struct McpSessionRegistry {
    sessions: HashMap<String, McpSession>,
    max_sessions: usize,
    ttl: SignedDuration,
}

impl McpSessionRegistry {
    #[must_use]
    pub fn new(max_sessions: usize, ttl: SignedDuration) -> Self {
        Self {
            sessions: HashMap::new(),
            max_sessions,
            ttl,
        }
    }

    pub fn initialize(
        &mut self,
        id: String,
        client_name: String,
        principal_key: String,
        now: Timestamp,
    ) -> McpServerResult<McpSession> {
        if !valid_session_id(&id) {
            return Err(McpServerError::InvalidSessionId);
        }
        self.reap(now);
        if self.sessions.len() >= self.max_sessions {
            return Err(McpServerError::SessionCapacityExceeded);
        }
        let session = McpSession {
            id: id.clone(),
            protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
            initialized_at: now,
            expires_at: now
                .checked_add(self.ttl)
                .map_err(|_| McpServerError::SessionCapacityExceeded)?,
            client_name,
            principal_key,
            initialized: false,
        };
        self.sessions.insert(id, session.clone());
        Ok(session)
    }

    pub fn require(
        &mut self,
        id: &str,
        protocol: &str,
        now: Timestamp,
    ) -> McpServerResult<McpSession> {
        self.reap(now);
        let session = self
            .sessions
            .get(id)
            .cloned()
            .ok_or(McpServerError::SessionUnknown)?;
        if protocol != session.protocol_version {
            return Err(McpServerError::ProtocolMismatch {
                expected: session.protocol_version,
                actual: protocol.to_owned(),
            });
        }
        Ok(session)
    }

    pub fn remove(&mut self, id: &str) -> McpServerResult<()> {
        self.sessions
            .remove(id)
            .map(|_| ())
            .ok_or(McpServerError::SessionUnknown)
    }

    pub fn mark_initialized(
        &mut self,
        id: &str,
        protocol: &str,
        now: Timestamp,
    ) -> McpServerResult<()> {
        self.reap(now);
        let session = self
            .sessions
            .get_mut(id)
            .ok_or(McpServerError::SessionUnknown)?;
        if protocol != session.protocol_version {
            return Err(McpServerError::ProtocolMismatch {
                expected: session.protocol_version.clone(),
                actual: protocol.to_owned(),
            });
        }
        session.initialized = true;
        Ok(())
    }

    pub fn require_initialized(
        &mut self,
        id: &str,
        protocol: &str,
        now: Timestamp,
    ) -> McpServerResult<McpSession> {
        let session = self.require(id, protocol, now)?;
        if !session.initialized {
            return Err(McpServerError::SessionNotInitialized);
        }
        Ok(session)
    }

    pub fn reap(&mut self, now: Timestamp) {
        self.sessions.retain(|_, session| now < session.expires_at);
    }
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    #[test]
    fn session_expiry_and_protocol_are_enforced() -> TestResult {
        let now = Timestamp::now();
        let mut sessions = McpSessionRegistry::new(1, SignedDuration::from_secs(1));
        let session = sessions
            .initialize(
                "session-a".to_owned(),
                "test".to_owned(),
                "principal-a".to_owned(),
                now,
            )
            .map_err(ctx("Session initialisieren"))?;
        assert!(
            sessions
                .require(&session.id, MCP_PROTOCOL_VERSION, now)
                .is_ok()
        );
        assert!(matches!(
            sessions.require(&session.id, "wrong", now),
            Err(McpServerError::ProtocolMismatch { .. })
        ));
        assert!(matches!(
            sessions.require_initialized(&session.id, MCP_PROTOCOL_VERSION, now),
            Err(McpServerError::SessionNotInitialized)
        ));
        sessions
            .mark_initialized(&session.id, MCP_PROTOCOL_VERSION, now)
            .map_err(ctx("Session als initialisiert markieren"))?;
        assert!(
            sessions
                .require_initialized(&session.id, MCP_PROTOCOL_VERSION, now)
                .is_ok()
        );
        let later = now
            .checked_add(SignedDuration::from_secs(1))
            .map_err(ctx("later timestamp in range"))?;
        assert!(matches!(
            sessions.require(&session.id, MCP_PROTOCOL_VERSION, later),
            Err(McpServerError::SessionUnknown)
        ));
        Ok(())
    }
}
