// Spec: CONTRACT_harw_browser.md, section `observation.rs`.

use crate::artifact::ArtifactRef;
use crate::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId,
};
use crate::selector::Selector;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ObservationMode {
    PageSummary,
    InteractiveElements,
    DomSelection { selector: Selector },
    TextExtraction,
    FormsAndLinks,
    NetworkActivitySummary,
    ConsoleLogSummary,
    Screenshot,
    CombinedDiagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocumentIdentity(String);

impl DocumentIdentity {
    pub fn new(identity: impl Into<String>) -> Self {
        Self(identity.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObservedElement {
    pub element_ref: String,
    pub tag: String,
    pub role: Option<String>,
    pub accessible_name: Option<String>,
    pub text: Option<String>,
    pub attributes: std::collections::BTreeMap<String, String>,
    pub bounding_box: Option<BoundingBox>,
}

impl ObservedElement {
    pub fn new(element_ref: impl Into<String>, tag: impl Into<String>) -> Self {
        Self {
            element_ref: element_ref.into(),
            tag: tag.into(),
            role: None,
            accessible_name: None,
            text: None,
            attributes: std::collections::BTreeMap::new(),
            bounding_box: None,
        }
    }
}

// Mirrors plan.md section 15.4 exactly — field names and order must match.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BrowserObservation {
    pub session_id: BrowserSessionId,
    pub context_id: BrowserContextId,
    pub revision: BrowserObservationRevision,
    pub url: url::Url,
    pub title: String,
    pub document_identity: DocumentIdentity,
    pub elements: Vec<ObservedElement>,
    pub artifacts: Vec<ArtifactRef>,
    pub event_cursor: BrowserEventCursor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactKind;

    #[test]
    fn test_document_identity_new_as_str_round_trip() {
        let identity = DocumentIdentity::new("x");
        assert_eq!(identity.as_str(), "x");
    }

    #[test]
    fn test_observed_element_new_defaults_are_empty() {
        let element = ObservedElement::new("ref-1", "div");
        assert_eq!(element.element_ref, "ref-1");
        assert_eq!(element.tag, "div");
        assert_eq!(element.role, None);
        assert_eq!(element.accessible_name, None);
        assert_eq!(element.text, None);
        assert!(element.attributes.is_empty());
        assert_eq!(element.bounding_box, None);
    }

    #[test]
    fn test_browser_observation_field_round_trip() {
        let session_id = BrowserSessionId::new();
        let context_id = BrowserContextId::new();
        let revision = BrowserObservationRevision::initial();
        let url = url::Url::parse("https://example.com/page").expect("valid url");
        let event_cursor = BrowserEventCursor::zero();
        let element = ObservedElement::new("ref-1", "button");
        let artifact = ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 128);

        let observation = BrowserObservation {
            session_id,
            context_id,
            revision,
            url: url.clone(),
            title: "Example".to_owned(),
            document_identity: DocumentIdentity::new("doc-1"),
            elements: vec![element.clone()],
            artifacts: vec![artifact.clone()],
            event_cursor,
        };

        assert_eq!(observation.session_id, session_id);
        assert_eq!(observation.context_id, context_id);
        assert_eq!(observation.revision, revision);
        assert_eq!(observation.url, url);
        assert_eq!(observation.title, "Example");
        assert_eq!(observation.document_identity.as_str(), "doc-1");
        assert_eq!(observation.elements, vec![element]);
        assert_eq!(observation.artifacts, vec![artifact]);
        assert_eq!(observation.event_cursor, event_cursor);
    }

    #[test]
    fn test_browser_observation_serde_json_round_trip() {
        let mut attributes = std::collections::BTreeMap::new();
        attributes.insert("data-testid".to_owned(), "submit".to_owned());

        let mut element = ObservedElement::new("ref-1", "button");
        element.role = Some("button".to_owned());
        element.accessible_name = Some("Submit".to_owned());
        element.text = Some("Submit".to_owned());
        element.attributes = attributes;
        element.bounding_box = Some(BoundingBox {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        });

        let observation = BrowserObservation {
            session_id: BrowserSessionId::new(),
            context_id: BrowserContextId::new(),
            revision: BrowserObservationRevision::initial().next(),
            url: url::Url::parse("https://example.com/checkout").expect("valid url"),
            title: "Checkout".to_owned(),
            document_identity: DocumentIdentity::new("doc-42"),
            elements: vec![element],
            artifacts: vec![ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 512)],
            event_cursor: BrowserEventCursor::zero().next(),
        };

        let json = serde_json::to_string(&observation).expect("observation serializes");
        let decoded: BrowserObservation =
            serde_json::from_str(&json).expect("observation deserializes");
        assert_eq!(decoded, observation);
    }

    #[test]
    fn test_observation_mode_dom_selection_serde_json_round_trip() {
        let mode = ObservationMode::DomSelection {
            selector: Selector::Css("main > article".to_owned()),
        };
        let json = serde_json::to_string(&mode).expect("mode serializes");
        let decoded: ObservationMode = serde_json::from_str(&json).expect("mode deserializes");
        assert_eq!(decoded, mode);
    }

    #[test]
    fn test_observation_mode_unit_variants_are_distinct() {
        assert_ne!(
            ObservationMode::PageSummary,
            ObservationMode::TextExtraction
        );
        assert_ne!(
            ObservationMode::Screenshot,
            ObservationMode::CombinedDiagnostic
        );
    }

    #[test]
    fn test_bounding_box_equality_and_serde_json_round_trip() {
        let a = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        };
        let b = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(a, b);

        let json = serde_json::to_string(&a).expect("bounding box serializes");
        let decoded: BoundingBox = serde_json::from_str(&json).expect("bounding box deserializes");
        assert_eq!(decoded, a);
    }
}
