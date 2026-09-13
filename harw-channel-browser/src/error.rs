use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors at the browser-channel configuration and coordination boundary.
#[derive(Debug)]
pub enum Error {
    ConnectorToml {
        source: toml::de::Error,
    },
    UnsupportedSchema {
        found: String,
    },
    UnsupportedBrowserBinding {
        found: String,
    },
    MissingField {
        field: &'static str,
    },
    EmptyField {
        field: &'static str,
    },
    InvalidUrl {
        field: &'static str,
        value: String,
        source: url::ParseError,
    },
    StartOriginNotAllowed {
        start_url: String,
    },
    PersistentProfileBindingMissing,
    Selector {
        location: &'static str,
        issue: SelectorIssue,
    },
    Confirmation {
        issue: ConfirmationIssue,
    },
    AutonomousSendBlocked {
        state: crate::takeover::TakeoverState,
    },
    DeliveryConfirmationMissingExternalId {
        source: DeliveryConfirmationSource,
    },
}

/// A precise declarative-selector compilation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorIssue {
    MissingStrategy,
    EmptyValue {
        strategy: SelectorStrategy,
    },
    AmbiguousStrategies {
        first: SelectorStrategy,
        second: SelectorStrategy,
    },
    EmptyFallback,
}

/// Supported selector keys in connector TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorStrategy {
    TestId,
    Css,
    Id,
    Name,
    Role,
    XPath,
    LinkText,
    TextAnchor,
    TagClass,
}

/// Invalid combinations in outbound delivery confirmation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationIssue {
    MissingNetworkRequestPath,
    MissingDomSelector,
    MissingNetworkAndDomEvidence,
    RequestPathMustBeAbsolute,
}

/// Which external observation claimed to confirm an outbound message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryConfirmationSource {
    Network,
    Dom,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectorToml { source } => {
                write!(
                    formatter,
                    "connector definition is not valid TOML: {source}"
                )
            }
            Self::UnsupportedSchema { found } => {
                write!(formatter, "unsupported connector schema `{found}`")
            }
            Self::UnsupportedBrowserBinding { found } => {
                write!(formatter, "unsupported browser binding `{found}`")
            }
            Self::MissingField { field } => {
                write!(formatter, "connector field `{field}` is required")
            }
            Self::EmptyField { field } => {
                write!(formatter, "connector field `{field}` must not be empty")
            }
            Self::InvalidUrl {
                field,
                value,
                source,
            } => write!(
                formatter,
                "connector field `{field}` contains invalid URL `{value}`: {source}"
            ),
            Self::StartOriginNotAllowed { start_url } => write!(
                formatter,
                "connector start URL `{start_url}` is not allowed by its origin policy"
            ),
            Self::PersistentProfileBindingMissing => formatter
                .write_str("persistent connector profile requires a non-empty profile binding"),
            Self::Selector { location, issue } => {
                write!(formatter, "invalid selector at `{location}`: {issue}")
            }
            Self::Confirmation { issue } => {
                write!(formatter, "invalid delivery confirmation policy: {issue}")
            }
            Self::AutonomousSendBlocked { state } => {
                write!(formatter, "autonomous send is blocked in state {state:?}")
            }
            Self::DeliveryConfirmationMissingExternalId { source } => write!(
                formatter,
                "{source} delivery confirmation did not provide an external message ID"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConnectorToml { source } => Some(source),
            Self::InvalidUrl { source, .. } => Some(source),
            Self::UnsupportedSchema { .. }
            | Self::UnsupportedBrowserBinding { .. }
            | Self::MissingField { .. }
            | Self::EmptyField { .. }
            | Self::StartOriginNotAllowed { .. }
            | Self::PersistentProfileBindingMissing
            | Self::Selector { .. }
            | Self::Confirmation { .. }
            | Self::AutonomousSendBlocked { .. }
            | Self::DeliveryConfirmationMissingExternalId { .. } => None,
        }
    }
}

impl From<toml::de::Error> for Error {
    fn from(source: toml::de::Error) -> Self {
        Self::ConnectorToml { source }
    }
}

impl fmt::Display for SelectorIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingStrategy => formatter.write_str("no selector strategy was provided"),
            Self::EmptyValue { strategy } => {
                write!(formatter, "{strategy} selector value must not be empty")
            }
            Self::AmbiguousStrategies { first, second } => write!(
                formatter,
                "selector specifies both {first} and {second}; exactly one strategy is allowed"
            ),
            Self::EmptyFallback => formatter.write_str("fallback selector must not be empty"),
        }
    }
}

impl fmt::Display for SelectorStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::TestId => "test_id",
            Self::Css => "css",
            Self::Id => "id",
            Self::Name => "name",
            Self::Role => "role",
            Self::XPath => "xpath",
            Self::LinkText => "link_text",
            Self::TextAnchor => "text_anchor",
            Self::TagClass => "tag_class",
        };
        formatter.write_str(name)
    }
}

impl fmt::Display for ConfirmationIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNetworkRequestPath => {
                formatter.write_str("network confirmation requires a request path")
            }
            Self::MissingDomSelector => {
                formatter.write_str("DOM confirmation requires an own-message selector")
            }
            Self::MissingNetworkAndDomEvidence => formatter
                .write_str("network-or-DOM confirmation requires at least one evidence source"),
            Self::RequestPathMustBeAbsolute => {
                formatter.write_str("confirmation request path must begin with `/`")
            }
        }
    }
}

impl fmt::Display for DeliveryConfirmationSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network => formatter.write_str("network"),
            Self::Dom => formatter.write_str("DOM"),
        }
    }
}
