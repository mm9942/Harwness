//! # action
//!
//! ## Responsibility
//! This module owns the **mutation surface**: the set of actions a caller can
//! request against a browser context ([`BrowserAction`]), the request
//! envelope that pairs an action with its target context and an optional
//! optimistic-concurrency guard ([`ActionRequest`]), and the outcome envelope
//! a backend returns after attempting the action ([`ActionOutcome`]). It does
//! not own selector resolution (see [`crate::selector`]), execution against a
//! real browser (see [`crate::host::BrowserRuntime::act`]), or failure
//! classification beyond attaching [`crate::diagnostic::BrowserDiagnostic`]
//! evidence to an outcome.
//!
//! ## Key types exported
//! - [`BrowserAction`] — the closed set of mutating operations a caller may
//!   request (click, type, navigate, drag, and so on).
//! - [`ActionRequest`] — an action bound to a [`crate::ids::BrowserContextId`]
//!   with an optional expected observation revision for optimistic
//!   concurrency.
//! - [`ActionOutcome`] — the result of attempting an action: the effect id,
//!   the resulting revision (if any), whether the effect was confirmed, and
//!   any diagnostics surfaced along the way.
//!
//! ## Concurrency
//! Single-threaded, pure data types. `Send + Sync` is derived automatically
//! since every field is itself `Send + Sync`; no locking or shared state is
//! introduced here.
//!
//! ## Errors
//! This module defines no fallible operations itself; failures produced while
//! executing a [`BrowserAction`] are reported by the backend as
//! [`crate::error::Error`] and, for partial/non-fatal issues, as entries in
//! [`ActionOutcome::diagnostics`].
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::action::{ActionRequest, BrowserAction};
//! use harw_browser::ids::BrowserContextId;
//! use harw_browser::selector::{Selector, Target};
//!
//! let context_id = BrowserContextId::new();
//! let action = BrowserAction::Click {
//!     target: Target::new(Selector::TestId("submit-button".to_owned())),
//! };
//! let request = ActionRequest::new(context_id, action);
//! assert!(request.expected_revision.is_none());
//! ```

// action.rs — see CONTRACT_harw_browser.md, section `action.rs`.

use crate::diagnostic::BrowserDiagnostic;
use crate::ids::{BrowserContextId, BrowserObservationRevision, EffectId};
use crate::selector::Target;

/// The closed set of mutating operations a caller may request against a
/// browser context.
///
/// # Description
/// Each variant carries exactly the data needed to perform one kind of
/// mutation: element-targeted interactions (`Click`, `Type`, `Clear`,
/// `Select`, `Focus`, `Hover`, `Drag`, `Upload`, `Submit`) carry one or more
/// [`Target`]s, viewport/keyboard interactions (`Scroll`, `KeyPress`) accept
/// an optional target (`None` means the action applies to the page as a
/// whole), and navigation actions (`Navigate`, `Back`, `Forward`, `Reload`)
/// carry only the data needed to navigate. This type is serialized across the
/// tool/model boundary, so every variant must remain representable in JSON.
///
/// # Concurrency
/// Plain data, `Send + Sync`, no interior mutability.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::action::BrowserAction;
/// use harw_browser::selector::{Selector, Target};
///
/// let action = BrowserAction::Type {
///     target: Target::new(Selector::Id("search".to_owned())),
///     text: "harwness".to_owned(),
/// };
/// assert!(matches!(action, BrowserAction::Type { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrowserAction {
    Click {
        target: Target,
    },
    Type {
        target: Target,
        text: String,
    },
    Clear {
        target: Target,
    },
    Select {
        target: Target,
        value: String,
    },
    Focus {
        target: Target,
    },
    Hover {
        target: Target,
    },
    Scroll {
        target: Option<Target>,
        x: i32,
        y: i32,
    },
    KeyPress {
        target: Option<Target>,
        key: String,
    },
    Drag {
        source: Target,
        destination: Target,
    },
    // `file_path` stays a plain String: this type crosses a serialization boundary to the
    // model/tool layer, and local upload handling converts it to a `Path` at the adapter layer.
    Upload {
        target: Target,
        file_path: String,
    },
    Submit {
        target: Target,
    },
    Navigate {
        url: url::Url,
    },
    Back,
    Forward,
    Reload,
}

/// A [`BrowserAction`] bound to the context it should run against, with an
/// optional optimistic-concurrency guard.
///
/// # Description
/// `context_id` identifies which browsing context the action targets.
/// `expected_revision`, when set via [`ActionRequest::with_expected_revision`],
/// asks the backend to reject the action with
/// [`crate::error::Error::StaleRevision`] if the context's current
/// [`crate::ids::BrowserObservationRevision`] has advanced past the expected
/// value since the caller last observed the page — preventing actions from
/// being applied against a page state the caller no longer has evidence of.
///
/// # Concurrency
/// Plain data, `Send + Sync`, no interior mutability.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::action::{ActionRequest, BrowserAction};
/// use harw_browser::ids::{BrowserContextId, BrowserObservationRevision};
///
/// let request = ActionRequest::new(BrowserContextId::new(), BrowserAction::Reload)
///     .with_expected_revision(BrowserObservationRevision::initial());
/// assert!(request.expected_revision.is_some());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionRequest {
    pub context_id: BrowserContextId,
    pub expected_revision: Option<BrowserObservationRevision>,
    pub action: BrowserAction,
}

impl ActionRequest {
    /// Builds a new request for `action` against `context_id` with no
    /// expected revision (no optimistic-concurrency check).
    ///
    /// # Arguments
    /// - `context_id` (`BrowserContextId`): the browsing context the action
    ///   targets. Owned (identifiers are `Copy`).
    /// - `action` (`BrowserAction`): the mutation to perform. Ownership is
    ///   transferred into the request.
    ///
    /// # Returns
    /// `ActionRequest` — the request with `expected_revision` set to `None`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::action::{ActionRequest, BrowserAction};
    /// use harw_browser::ids::BrowserContextId;
    ///
    /// let request = ActionRequest::new(BrowserContextId::new(), BrowserAction::Back);
    /// assert_eq!(request.expected_revision, None);
    /// ```
    pub fn new(context_id: BrowserContextId, action: BrowserAction) -> Self {
        Self {
            context_id,
            expected_revision: None,
            action,
        }
    }

    /// Attaches an expected observation revision to enforce optimistic
    /// concurrency.
    ///
    /// # Description
    /// Consumes and returns `self` so calls can be chained onto
    /// [`ActionRequest::new`]. A backend that receives a request with this
    /// field set must reject it with
    /// [`crate::error::Error::StaleRevision`] if the context's current
    /// revision no longer matches.
    ///
    /// # Arguments
    /// - `revision` (`BrowserObservationRevision`): the revision the caller
    ///   last observed and expects to still be current. Owned (`Copy`).
    ///
    /// # Returns
    /// `ActionRequest` — `self` with `expected_revision` set to
    /// `Some(revision)`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::action::{ActionRequest, BrowserAction};
    /// use harw_browser::ids::{BrowserContextId, BrowserObservationRevision};
    ///
    /// let request = ActionRequest::new(BrowserContextId::new(), BrowserAction::Forward)
    ///     .with_expected_revision(BrowserObservationRevision::initial());
    /// assert_eq!(request.expected_revision, Some(BrowserObservationRevision::initial()));
    /// ```
    pub fn with_expected_revision(mut self, revision: BrowserObservationRevision) -> Self {
        self.expected_revision = Some(revision);
        self
    }
}

/// The result of a backend attempting an [`ActionRequest`].
///
/// # Description
/// `effect_id` uniquely identifies this attempted effect so it can be
/// referenced later (for example by a [`crate::diagnostic::BrowserDiagnostic`]
/// raised asynchronously). `new_revision` is the context's observation
/// revision after the action, if the backend was able to determine one.
/// `confirmed` records whether the backend positively verified the action
/// took effect (as opposed to merely dispatching it). `diagnostics` carries
/// any non-fatal warnings or failure evidence gathered while performing the
/// action.
///
/// # Concurrency
/// Plain data, `Send + Sync`, no interior mutability.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::action::ActionOutcome;
/// use harw_browser::ids::EffectId;
///
/// let outcome = ActionOutcome::new(EffectId::new());
/// assert!(!outcome.confirmed);
/// assert!(outcome.diagnostics.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActionOutcome {
    pub effect_id: EffectId,
    pub new_revision: Option<BrowserObservationRevision>,
    pub confirmed: bool,
    pub diagnostics: Vec<BrowserDiagnostic>,
}

impl ActionOutcome {
    /// Builds a fresh, unconfirmed outcome for `effect_id` with no known
    /// resulting revision and no diagnostics.
    ///
    /// # Arguments
    /// - `effect_id` (`EffectId`): the identifier assigned to this attempted
    ///   effect. Owned (`Copy`).
    ///
    /// # Returns
    /// `ActionOutcome` — `new_revision: None`, `confirmed: false`,
    /// `diagnostics: Vec::new()`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::action::ActionOutcome;
    /// use harw_browser::ids::EffectId;
    ///
    /// let outcome = ActionOutcome::new(EffectId::new());
    /// assert_eq!(outcome.new_revision, None);
    /// ```
    pub fn new(effect_id: EffectId) -> Self {
        Self {
            effect_id,
            new_revision: None,
            confirmed: false,
            diagnostics: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::Selector;

    #[test]
    fn test_action_request_new_has_no_expected_revision() {
        let context_id = BrowserContextId::new();
        let action = BrowserAction::Back;
        let request = ActionRequest::new(context_id, action);
        assert_eq!(request.context_id, context_id);
        assert_eq!(request.expected_revision, None);
        assert_eq!(request.action, BrowserAction::Back);
    }

    #[test]
    fn test_action_request_with_expected_revision_sets_field() {
        let context_id = BrowserContextId::new();
        let revision = BrowserObservationRevision::initial().next();
        let request =
            ActionRequest::new(context_id, BrowserAction::Forward).with_expected_revision(revision);
        assert_eq!(request.expected_revision, Some(revision));
    }

    #[test]
    fn test_action_outcome_new_has_expected_defaults() {
        let effect_id = EffectId::new();
        let outcome = ActionOutcome::new(effect_id);
        assert_eq!(outcome.effect_id, effect_id);
        assert_eq!(outcome.new_revision, None);
        assert!(!outcome.confirmed);
        assert!(outcome.diagnostics.is_empty());
    }

    #[test]
    fn test_browser_action_variants_partial_eq() {
        let target_a = Target::new(Selector::Id("a".to_owned()));
        let target_b = Target::new(Selector::Id("b".to_owned()));

        let click_a1 = BrowserAction::Click {
            target: target_a.clone(),
        };
        let click_a2 = BrowserAction::Click {
            target: target_a.clone(),
        };
        let click_b = BrowserAction::Click {
            target: target_b.clone(),
        };
        assert_eq!(click_a1, click_a2);
        assert_ne!(click_a1, click_b);

        let type_a1 = BrowserAction::Type {
            target: target_a.clone(),
            text: "hello".to_owned(),
        };
        let type_a2 = BrowserAction::Type {
            target: target_a.clone(),
            text: "hello".to_owned(),
        };
        let type_a3 = BrowserAction::Type {
            target: target_a.clone(),
            text: "world".to_owned(),
        };
        assert_eq!(type_a1, type_a2);
        assert_ne!(type_a1, type_a3);

        let scroll_1 = BrowserAction::Scroll {
            target: None,
            x: 0,
            y: 100,
        };
        let scroll_2 = BrowserAction::Scroll {
            target: None,
            x: 0,
            y: 100,
        };
        let scroll_3 = BrowserAction::Scroll {
            target: Some(target_a.clone()),
            x: 0,
            y: 100,
        };
        assert_eq!(scroll_1, scroll_2);
        assert_ne!(scroll_1, scroll_3);
        assert_ne!(scroll_1, click_a1);
    }

    #[test]
    fn test_navigate_action_holds_url() {
        let url = url::Url::parse("https://example.com/").expect("valid url in test");
        let navigate_1 = BrowserAction::Navigate { url: url.clone() };
        let navigate_2 = BrowserAction::Navigate { url };
        assert_eq!(navigate_1, navigate_2);
        assert_ne!(navigate_1, BrowserAction::Reload);
    }

    #[test]
    fn test_browser_action_serde_json_round_trip() {
        let actions = vec![
            BrowserAction::Click {
                target: Target::new(Selector::Css("button.submit".to_owned())),
            },
            BrowserAction::Upload {
                target: Target::new(Selector::Id("file-input".to_owned())),
                file_path: "/tmp/upload.txt".to_owned(),
            },
            BrowserAction::Navigate {
                url: url::Url::parse("https://example.com/page").expect("valid url in test"),
            },
        ];

        for action in actions {
            let json = serde_json::to_string(&action).expect("action serializes");
            let decoded: BrowserAction = serde_json::from_str(&json).expect("action deserializes");
            assert_eq!(decoded, action);
        }
    }

    #[test]
    fn test_action_request_serde_json_round_trip() {
        let request = ActionRequest::new(
            BrowserContextId::new(),
            BrowserAction::Submit {
                target: Target::new(Selector::Name("form".to_owned())),
            },
        )
        .with_expected_revision(BrowserObservationRevision::initial());

        let json = serde_json::to_string(&request).expect("request serializes");
        let decoded: ActionRequest = serde_json::from_str(&json).expect("request deserializes");
        assert_eq!(decoded, request);
    }

    #[test]
    fn test_action_outcome_serde_json_round_trip_with_diagnostics() {
        let diagnostic = BrowserDiagnostic::new(
            crate::diagnostic::Severity::Low,
            crate::diagnostic::DiagnosticSource::Selector,
            crate::diagnostic::DiagnosticCategory::SelectorStale,
            "element moved before click",
        );
        let mut outcome = ActionOutcome::new(EffectId::new());
        outcome.new_revision = Some(BrowserObservationRevision::initial().next());
        outcome.confirmed = true;
        outcome.diagnostics.push(diagnostic);

        let json = serde_json::to_string(&outcome).expect("outcome serializes");
        let decoded: ActionOutcome = serde_json::from_str(&json).expect("outcome deserializes");
        assert_eq!(decoded, outcome);
    }
}
