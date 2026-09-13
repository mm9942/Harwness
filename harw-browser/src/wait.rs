use crate::selector::Target;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WaitCondition {
    ElementPresent(Target),
    ElementVisible(Target),
    ElementClickable(Target),
    ElementGone(Target),
    UrlMatches(String),
    TitleMatches(String),
    NavigationComplete,
    NetworkQuiescence { idle_ms: u64 },
    RequestObserved { path_contains: String },
    LogMatches(String),
    ScriptMessage { channel: String },
    DownloadComplete,
    CustomScript { predicate: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WaitTimeout(std::time::Duration);

impl WaitTimeout {
    pub fn from_millis(millis: u64) -> Self {
        Self(std::time::Duration::from_millis(millis))
    }

    pub fn duration(&self) -> std::time::Duration {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WaitOutcome {
    pub satisfied: bool,
    pub elapsed_ms: u64,
    pub condition: WaitCondition,
}

impl WaitOutcome {
    pub fn new(satisfied: bool, elapsed_ms: u64, condition: WaitCondition) -> Self {
        Self {
            satisfied,
            elapsed_ms,
            condition,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::Selector;
    use std::time::Duration;

    #[test]
    fn test_wait_timeout_from_millis_duration_matches() {
        assert_eq!(
            WaitTimeout::from_millis(500).duration(),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn test_wait_condition_variants_equality() {
        let a = WaitCondition::NavigationComplete;
        let b = WaitCondition::NavigationComplete;
        assert_eq!(a, b);

        let c = WaitCondition::UrlMatches("https://example.com".to_owned());
        let d = WaitCondition::UrlMatches("https://example.com".to_owned());
        assert_eq!(c, d);

        let e = WaitCondition::NetworkQuiescence { idle_ms: 250 };
        let f = WaitCondition::NetworkQuiescence { idle_ms: 250 };
        assert_eq!(e, f);
    }

    #[test]
    fn test_wait_condition_variants_inequality() {
        let a = WaitCondition::LogMatches("error".to_owned());
        let b = WaitCondition::LogMatches("warning".to_owned());
        assert_ne!(a, b);

        let c = WaitCondition::DownloadComplete;
        let d = WaitCondition::NavigationComplete;
        assert_ne!(c, d);
    }

    #[test]
    fn test_wait_outcome_field_access() {
        let outcome = WaitOutcome::new(true, 120, WaitCondition::DownloadComplete);
        assert!(outcome.satisfied);
        assert_eq!(outcome.elapsed_ms, 120);
        assert_eq!(outcome.condition, WaitCondition::DownloadComplete);
    }

    #[test]
    fn test_wait_timeout_serde_json_round_trip() {
        let timeout = WaitTimeout::from_millis(2500);
        let json = serde_json::to_string(&timeout).expect("timeout serializes");
        let decoded: WaitTimeout = serde_json::from_str(&json).expect("timeout deserializes");
        assert_eq!(decoded, timeout);
    }

    #[test]
    fn test_wait_outcome_serde_json_round_trip_with_element_target() {
        let outcome = WaitOutcome::new(
            false,
            4000,
            WaitCondition::ElementClickable(Target::new(Selector::TestId("submit".to_owned()))),
        );

        let json = serde_json::to_string(&outcome).expect("outcome serializes");
        let decoded: WaitOutcome = serde_json::from_str(&json).expect("outcome deserializes");
        assert_eq!(decoded, outcome);
    }
}
