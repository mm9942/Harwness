//! Fallible Onion operations and stable diagnostic stages.

use std::time::Duration;

/// Stage at which an Onion-specific failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnionStage {
    /// Parse the endpoint or configure Tor.
    Configuration,
    /// Create the process runtime and Tor client.
    Runtime,
    /// Obtain enough Tor network information to proceed.
    Bootstrap,
    /// The running Tor implementation is no longer supported by the network.
    Compatibility,
    /// Load or verify a pre-existing Onion service key.
    Identity,
    /// Initialize the persistent Onion service.
    Service,
    /// Establish an outbound Onion stream.
    Dial,
}

/// Errors of the onion transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OnionError {
    /// The locator text is not a valid v3 onion locator.
    #[error("invalid onion locator: {0}")]
    InvalidLocator(&'static str),
    /// The Tor client configuration could not be built.
    #[error("tor configuration: {0}")]
    Config(String),
    /// No rustls crypto provider is available for Tor.
    #[error("no rustls crypto provider is available for Tor")]
    CryptoProvider,
    /// The Tor runtime or client could not be created.
    #[error("tor runtime: {0}")]
    Runtime(String),
    /// Tor bootstrap failed.
    #[error("tor bootstrap failed: {0}")]
    Bootstrap(String),
    /// Tor bootstrap did not finish in time.
    #[error("tor bootstrap did not finish within {0:?}")]
    BootstrapTimeout(Duration),
    /// This build's Tor is too old for the current Tor network.
    #[error("this build's Tor (arti 0.48) is too old for the current Tor network; update harw")]
    Outdated,
    /// The configured persistent Onion identity is missing or mismatched.
    #[error("onion identity: {0}")]
    Identity(&'static str),
    /// The onion service failed.
    #[error("onion service: {0}")]
    Service(String),
    /// Connecting to an onion service failed.
    #[error("onion connect failed: {0}")]
    Connect(String),
    /// Connecting to an onion service did not finish in time.
    #[error("onion connect did not finish within {0:?}")]
    ConnectTimeout(Duration),
}

impl OnionError {
    /// Classifies failures without inspecting implementation-specific error text.
    #[must_use]
    pub const fn stage(&self) -> OnionStage {
        match self {
            Self::InvalidLocator(_) | Self::Config(_) => OnionStage::Configuration,
            Self::CryptoProvider | Self::Runtime(_) => OnionStage::Runtime,
            Self::Bootstrap(_) | Self::BootstrapTimeout(_) => OnionStage::Bootstrap,
            Self::Outdated => OnionStage::Compatibility,
            Self::Identity(_) => OnionStage::Identity,
            Self::Service(_) => OnionStage::Service,
            Self::Connect(_) | Self::ConnectTimeout(_) => OnionStage::Dial,
        }
    }
}
