//! Bekannte Methoden- und Notification-Namen für den Wire-Transport.

pub const METHOD_TURN_SUBMIT: &str = "turn.submit";
pub const METHOD_TURN_INTERRUPT: &str = "turn.interrupt";
pub const METHOD_APPROVAL_RESP: &str = "approval.respond";
pub const METHOD_SESSION_CLOSE: &str = "session.close";

pub const NOTIF_SESSION_EVENT: &str = "event.session";
pub const NOTIF_TURN_EVENT: &str = "event.turn";
pub const NOTIF_APPROVAL_REQ: &str = "approval.request";
