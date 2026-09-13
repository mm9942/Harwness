// Spec: CONTRACT_harw_browser.md, section `ids.rs`.

// Generates a Uuid-backed newtype with a random constructor, an accessor to the
// inner Uuid, and Display/FromStr delegating to the inner Uuid's implementations.
macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub struct $name(uuid::Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            pub fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(uuid::Uuid::from_str(s)?))
            }
        }
    };
}

uuid_id!(BrowserSessionId);
uuid_id!(BrowserContextId);
uuid_id!(EffectId);
uuid_id!(ArtifactId);
uuid_id!(EventId);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct BrowserObservationRevision(u64);

impl BrowserObservationRevision {
    pub fn initial() -> Self {
        Self(0)
    }

    pub fn value(&self) -> u64 {
        self.0
    }

    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct BrowserEventCursor(u64);

impl BrowserEventCursor {
    pub fn zero() -> Self {
        Self(0)
    }

    pub fn value(&self) -> u64 {
        self.0
    }

    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_browser_session_id_new_distinct() {
        assert_ne!(BrowserSessionId::new(), BrowserSessionId::new());
    }

    #[test]
    fn test_browser_context_id_new_distinct() {
        assert_ne!(BrowserContextId::new(), BrowserContextId::new());
    }

    #[test]
    fn test_effect_id_new_distinct() {
        assert_ne!(EffectId::new(), EffectId::new());
    }

    #[test]
    fn test_artifact_id_new_distinct() {
        assert_ne!(ArtifactId::new(), ArtifactId::new());
    }

    #[test]
    fn test_event_id_new_distinct() {
        assert_ne!(EventId::new(), EventId::new());
    }

    #[test]
    fn test_browser_session_id_display_from_str_round_trip() {
        let id = BrowserSessionId::new();
        let rendered = id.to_string();
        let parsed = BrowserSessionId::from_str(&rendered).expect("valid uuid string");
        assert_eq!(id, parsed);
        assert_eq!(id.as_uuid(), parsed.as_uuid());
    }

    #[test]
    fn test_browser_observation_revision_initial_is_zero() {
        assert_eq!(BrowserObservationRevision::initial().value(), 0);
    }

    #[test]
    fn test_browser_observation_revision_next_increments() {
        assert_eq!(BrowserObservationRevision::initial().next().value(), 1);
    }

    #[test]
    fn test_browser_event_cursor_zero_is_zero() {
        assert_eq!(BrowserEventCursor::zero().value(), 0);
    }

    #[test]
    fn test_browser_event_cursor_next_increments() {
        assert_eq!(BrowserEventCursor::zero().next().value(), 1);
    }

    #[test]
    fn test_uuid_ids_default_matches_new_shape() {
        // `Default` delegates to `new()`, which must produce a fresh random id each
        // call rather than a fixed sentinel value.
        assert_ne!(BrowserSessionId::default(), BrowserSessionId::default());
        assert_ne!(BrowserContextId::default(), BrowserContextId::default());
        assert_ne!(EffectId::default(), EffectId::default());
        assert_ne!(ArtifactId::default(), ArtifactId::default());
        assert_ne!(EventId::default(), EventId::default());
    }

    #[test]
    fn test_uuid_ids_from_str_invalid_returns_err() {
        assert!(BrowserSessionId::from_str("not-a-uuid").is_err());
        assert!(BrowserContextId::from_str("not-a-uuid").is_err());
        assert!(EffectId::from_str("not-a-uuid").is_err());
        assert!(ArtifactId::from_str("not-a-uuid").is_err());
        assert!(EventId::from_str("not-a-uuid").is_err());
    }

    #[test]
    fn test_browser_context_id_display_from_str_round_trip() {
        let id = BrowserContextId::new();
        let rendered = id.to_string();
        let parsed = BrowserContextId::from_str(&rendered).expect("valid uuid string");
        assert_eq!(id, parsed);
        assert_eq!(id.as_uuid(), parsed.as_uuid());
    }

    #[test]
    fn test_browser_observation_revision_ordering() {
        let initial = BrowserObservationRevision::initial();
        let next = initial.next();
        assert!(initial < next);
        assert!(next > initial);
    }

    #[test]
    fn test_browser_event_cursor_ordering() {
        let zero = BrowserEventCursor::zero();
        let next = zero.next();
        assert!(zero < next);
        assert!(next > zero);
    }
}
