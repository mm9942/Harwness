//! `harw-protocol` — Wire-Envelope und Event-/Item-Typen.
//!
//! Baut auf `harw-types` auf. Keine Runtime-Abhängigkeiten (kein tokio,
//! reqwest, Provider-SDKs). Neue Varianten sind additive Changes.

#![forbid(unsafe_code)]

pub mod approvals;
pub mod events;
pub mod items;
pub mod methods;
pub mod orchestration;
pub mod session_port;
pub mod session_wire;
pub mod wire;

pub use approvals::{ApprovalKind, ApprovalRequest, ApprovalResponse};
pub use events::{SessionEvent, TurnEvent};
pub use items::{
    AssistantMessageItem, ContentPart, ErrorItem, OpaqueReasoning, ReasoningItem, ResultTrust,
    ToolCallItem, ToolCallResult, ToolResultItem, TurnItem, UserMessageItem,
};
pub use orchestration::{AgentOrchestrationEvent, AgentOrchestrationStatus};
pub use session_port::{FrameSource, PortError, PortFuture, SessionPort};
pub use session_wire::{
    ClientCaps, Cursor, FrameEnvelope, HostedState, PresenceEntry, SessionFrame, StreamProfile,
};
pub use wire::{
    NotificationEnvelope, ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireError,
    WireMessage,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
