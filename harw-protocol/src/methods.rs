//! Bekannte Methoden- und Notification-Namen für den Wire-Transport.

pub const METHOD_TURN_SUBMIT: &str = "turn.submit";
pub const METHOD_TURN_INTERRUPT: &str = "turn.interrupt";
pub const METHOD_APPROVAL_RESP: &str = "approval.respond";
pub const METHOD_SESSION_CLOSE: &str = "session.close";
pub const METHOD_SESSION_HELLO: &str = "session.hello";
pub const METHOD_SESSION_LIST: &str = "session.list";
pub const METHOD_SESSION_CREATE: &str = "session.create";
pub const METHOD_SESSION_ATTACH: &str = "session.attach";
pub const METHOD_SESSION_DETACH: &str = "session.detach";
pub const METHOD_SESSION_HISTORY: &str = "session.history";
pub const METHOD_SESSION_RESUME: &str = "session.resume";
pub const METHOD_SESSION_SET_MODEL: &str = "session.set_model";
pub const METHOD_SESSION_SET_MODE: &str = "session.set_mode";
pub const METHOD_SESSION_SET_EFFORT: &str = "session.set_effort";

// R18 D-A: Werkzeugaufrufe über das Gateway, nur für Agent-Principals
// (Vertrag `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`).
pub const METHOD_TOOL_LIST: &str = "tool.list";
pub const METHOD_TOOL_CALL: &str = "tool.call";
pub const METHOD_TOOL_CANCEL: &str = "tool.cancel";

// R18 D-B: Gateway lesen (`gateway_read`) und verwalten (`gateway_admin`).
pub const METHOD_GATEWAY_STATUS: &str = "gateway.status";
pub const METHOD_GATEWAY_CONNECTIONS_LIST: &str = "gateway.connections.list";
pub const METHOD_GATEWAY_SESSIONS_LIST: &str = "gateway.sessions.list";
pub const METHOD_GATEWAY_LISTENERS_LIST: &str = "gateway.listeners.list";
pub const METHOD_GATEWAY_TOOLS_LIST: &str = "gateway.tools.list";
pub const METHOD_GATEWAY_CONNECTIONS_REVOKE: &str = "gateway.connections.revoke";
pub const METHOD_GATEWAY_DRAIN: &str = "gateway.drain";
pub const METHOD_GATEWAY_LISTENERS_SET: &str = "gateway.listeners.set";
pub const METHOD_GATEWAY_TOOLS_GRANT: &str = "gateway.tools.grant";
pub const METHOD_GATEWAY_TOOLS_NARROW: &str = "gateway.tools.narrow";

pub const NOTIF_SESSION_EVENT: &str = "event.session";
pub const NOTIF_TURN_EVENT: &str = "event.turn";
pub const NOTIF_APPROVAL_REQ: &str = "approval.request";
/// One [`crate::session_wire::FrameEnvelope`] of an attached session.
pub const NOTIF_EVENT_FRAME: &str = "event.frame";

/// Every method of the closed session control plane table (W00 D5).
pub const SESSION_METHODS: &[&str] = &[
    METHOD_SESSION_HELLO,
    METHOD_SESSION_LIST,
    METHOD_SESSION_CREATE,
    METHOD_SESSION_ATTACH,
    METHOD_SESSION_DETACH,
    METHOD_SESSION_HISTORY,
    METHOD_SESSION_RESUME,
    METHOD_SESSION_CLOSE,
    METHOD_SESSION_SET_MODEL,
    METHOD_SESSION_SET_MODE,
    METHOD_SESSION_SET_EFFORT,
    METHOD_TURN_SUBMIT,
    METHOD_TURN_INTERRUPT,
    METHOD_APPROVAL_RESP,
];

/// R18 D-A: the closed `tool.*` method table.
pub const TOOL_METHODS: &[&str] = &[METHOD_TOOL_LIST, METHOD_TOOL_CALL, METHOD_TOOL_CANCEL];

/// R18 D-B: read-only `gateway.*` methods (cap `gateway_read`).
pub const GATEWAY_READ_METHODS: &[&str] = &[
    METHOD_GATEWAY_STATUS,
    METHOD_GATEWAY_CONNECTIONS_LIST,
    METHOD_GATEWAY_SESSIONS_LIST,
    METHOD_GATEWAY_LISTENERS_LIST,
    METHOD_GATEWAY_TOOLS_LIST,
];

/// R18 D-B: mutating `gateway.*` methods (cap `gateway_admin`).
pub const GATEWAY_ADMIN_METHODS: &[&str] = &[
    METHOD_GATEWAY_CONNECTIONS_REVOKE,
    METHOD_GATEWAY_DRAIN,
    METHOD_GATEWAY_LISTENERS_SET,
    METHOD_GATEWAY_TOOLS_GRANT,
    METHOD_GATEWAY_TOOLS_NARROW,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Die drei Tabellen sind disjunkt, und kein Name ist doppelt: ein
    /// Dispatcher ordnet jeden Namen genau einer Fähigkeit zu.
    #[test]
    fn method_tables_are_disjoint_and_unique() {
        let mut all: Vec<&str> = SESSION_METHODS
            .iter()
            .chain(TOOL_METHODS)
            .chain(GATEWAY_READ_METHODS)
            .chain(GATEWAY_ADMIN_METHODS)
            .copied()
            .collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total);
        assert!(TOOL_METHODS.iter().all(|name| name.starts_with("tool.")));
        assert!(
            GATEWAY_READ_METHODS
                .iter()
                .chain(GATEWAY_ADMIN_METHODS)
                .all(|name| name.starts_with("gateway."))
        );
        // Schlüsseloperationen bleiben außerhalb jeder Gateway-Fläche.
        assert!(!all.iter().any(|name| name.contains("keys")));
    }
}
