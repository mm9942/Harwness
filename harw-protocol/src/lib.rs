//! `harw-protocol` — Wire-Envelope und Event-/Item-Typen.
//!
//! Baut auf `harw-types` auf. Keine Runtime-Abhängigkeiten (kein tokio,
//! reqwest, Provider-SDKs). Neue Varianten sind additive Changes.

#![forbid(unsafe_code)]

pub mod approvals;
pub mod events;
pub mod items;
pub mod methods;
pub mod wire;

pub use approvals::{ApprovalRequest, ApprovalResponse};
pub use events::{SessionEvent, TurnEvent};
pub use items::{
    AssistantMessageItem, ContentPart, ErrorItem, OpaqueReasoning, ReasoningItem, ResultTrust,
    ToolCallItem, ToolCallResult, ToolResultItem, TurnItem, UserMessageItem,
};
pub use wire::{
    NotificationEnvelope, ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireError,
    WireMessage,
};
