#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Selector {
    TestId(String),
    Css(String),
    Id(String),
    Name(String),
    TagClass { tag: String, class: String },
    LinkText(String),
    XPath(String),
    TextAnchor(String),
    Role { role: String, name: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub primary: Selector,
    pub fallbacks: Vec<Selector>,
}

impl Target {
    pub fn new(primary: Selector) -> Self {
        Self {
            primary,
            fallbacks: Vec::new(),
        }
    }

    pub fn with_fallback(mut self, fallback: Selector) -> Self {
        self.fallbacks.push(fallback);
        self
    }

    // primary first, then fallbacks in order
    pub fn candidates(&self) -> impl Iterator<Item = &Selector> {
        std::iter::once(&self.primary).chain(self.fallbacks.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_candidates_yields_primary_then_fallbacks_in_order() {
        let target = Target::new(Selector::Css("div.a".to_owned()))
            .with_fallback(Selector::Id("b".to_owned()))
            .with_fallback(Selector::Name("c".to_owned()));

        let candidates: Vec<&Selector> = target.candidates().collect();

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0], &Selector::Css("div.a".to_owned()));
        assert_eq!(candidates[1], &Selector::Id("b".to_owned()));
        assert_eq!(candidates[2], &Selector::Name("c".to_owned()));
    }

    #[test]
    fn test_with_fallback_chaining_builds_up_list() {
        let target = Target::new(Selector::TestId("root".to_owned()))
            .with_fallback(Selector::XPath("//div".to_owned()))
            .with_fallback(Selector::LinkText("click me".to_owned()))
            .with_fallback(Selector::TextAnchor("anchor".to_owned()));

        assert_eq!(target.primary, Selector::TestId("root".to_owned()));
        assert_eq!(
            target.fallbacks,
            vec![
                Selector::XPath("//div".to_owned()),
                Selector::LinkText("click me".to_owned()),
                Selector::TextAnchor("anchor".to_owned()),
            ]
        );
    }

    #[test]
    fn test_selector_equality_same_construction() {
        let a = Selector::Role {
            role: "button".to_owned(),
            name: Some("Submit".to_owned()),
        };
        let b = Selector::Role {
            role: "button".to_owned(),
            name: Some("Submit".to_owned()),
        };
        assert_eq!(a, b);

        let c = Selector::TagClass {
            tag: "div".to_owned(),
            class: "container".to_owned(),
        };
        let d = Selector::TagClass {
            tag: "div".to_owned(),
            class: "container".to_owned(),
        };
        assert_eq!(c, d);
    }

    #[test]
    fn test_target_serde_json_round_trip_with_struct_variant_selectors() -> TestResult {
        let target = Target::new(Selector::Role {
            role: "button".to_owned(),
            name: Some("Submit".to_owned()),
        })
        .with_fallback(Selector::TagClass {
            tag: "button".to_owned(),
            class: "primary".to_owned(),
        })
        .with_fallback(Selector::XPath("//button[@type='submit']".to_owned()));

        let json = serde_json::to_string(&target).map_err(ctx("target serializes"))?;
        let decoded: Target = serde_json::from_str(&json).map_err(ctx("target deserializes"))?;
        assert_eq!(decoded, target);
        Ok(())
    }

    #[test]
    fn test_selector_deserialize_rejects_unknown_field_in_struct_variant() {
        let json = serde_json::json!({
            "TagClass": { "tag": "div", "class": "container", "extra": "nope" }
        });

        let result: Result<Selector, serde_json::Error> = serde_json::from_value(json);

        assert!(result.is_err());
    }

    #[test]
    fn test_target_deserialize_rejects_unknown_field() {
        let json = serde_json::json!({
            "primary": { "Css": "div.a" },
            "fallbacks": [],
            "extra": "nope"
        });

        let result: Result<Target, serde_json::Error> = serde_json::from_value(json);

        assert!(result.is_err());
    }
}
