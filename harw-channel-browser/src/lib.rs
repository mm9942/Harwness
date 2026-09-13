//! Browser-backed channel perimeter for declarative external connectors.

pub mod bridge;
pub mod connector;
pub mod delivery;
pub mod error;
pub mod lease;
pub mod message;
pub mod runtime;
pub mod takeover;

pub use bridge::{
    BridgeConfigError, BridgeDefinition, BridgeIngestOutcome, BridgeMessage, BridgePolicy,
    BridgeProvenance, BridgeRejection, ContentTrust, PageBridgeIngestor,
};
pub use connector::{
    ActivityPolicy, CompiledConnector, Composer, ConfirmationMode, ConfirmationPolicy,
    ConnectorDefinition, MessageBinding, OriginPolicy, Profile, ProfileMode, SelectorBinding,
};
pub use delivery::{DeliveryEvidence, DeliveryState, PendingDelivery};
pub use error::{
    ConfirmationIssue, DeliveryConfirmationSource, Error, Result, SelectorIssue, SelectorStrategy,
};
pub use lease::{ConnectorLease, LeaseToken, LeaseValidationError};
pub use message::{
    ChannelMessage, MessageContent, MessageIdentity, MessageNormalizer, RawChannelMessage,
};
pub use runtime::{
    BrowserChannelRuntime, InboundDisposition, RuntimeError, RuntimeEvent, WakeNotification,
    WakeSink,
};
pub use takeover::{AutonomousSend, TakeoverEvent, TakeoverState};
