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
