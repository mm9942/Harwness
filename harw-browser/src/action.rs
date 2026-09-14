//! # action
//!
//! ## Responsibility
//! This module owns the **mutation surface**: the set of actions a caller can
//! request against a browser context ([`BrowserAction`]), the request
//! envelope that pairs an action with its target context and an optional
//! optimistic-concurrency guard ([`ActionRequest`]), the outcome envelope
//! a backend returns after attempting the action ([`ActionOutcome`]), the
//! limit validation for actions and selectors ([`BrowserAction::validate`],
//! [`validate_target`], [`validate_selector`]) and the per-session action
//! counter ([`ActionBudget`]). It does not own selector resolution (see
//! [`crate::selector`]), execution against a real browser (see
//! [`crate::host::BrowserRuntime::act`]), or origin enforcement (see
//! [`crate::policy::OpenBrowserRequest::check_navigation_target`] and
//! [`crate::policy::OpenBrowserRequest::check_observed_location`]).
//!
//! ## Security notes (remediation C-BROWSER, F-007)
//! - There is **no upload action**: a model-steerable action that reads local
//!   files is not representable. JSON naming `Upload`/`upload` is rejected
//!   by deserialization (unknown variant).
//! - All struct variants reject unknown fields (`deny_unknown_fields`).
//! - Serde acceptance is not validation: callers must run
//!   [`ActionRequest::validate`] with the session's
//!   [`crate::policy::BrowserLimits`] before dispatch.
//!
//! ## Key types exported
//! - [`BrowserAction`] — the closed set of mutating operations a caller may
//!   request (click, type, navigate, drag, and so on).
//! - [`ActionRequest`] — an action bound to a [`crate::ids::BrowserContextId`]
//!   with an optional expected observation revision for optimistic
//!   concurrency.
//! - [`ActionOutcome`] — the result of attempting an action.
//! - [`ActionBudget`] — counts actions against
//!   [`crate::policy::BrowserLimits::max_actions_per_session`].
//!
//! ## Concurrency
//! Pure data types and pure functions, `Send + Sync`. [`ActionBudget`] is
//! mutated through `&mut self`; a backend sharing it across tasks must wrap
//! it in its own lock.
//!
//! ## Errors
//! - [`crate::error::Error::InvalidArgument`]: an action, selector or budget
//!   limit is violated.
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::action::{ActionRequest, BrowserAction};
//! use harw_browser::ids::BrowserContextId;
//! use harw_browser::policy::BrowserLimits;
//! use harw_browser::selector::{Selector, Target};
//!
//! let context_id = BrowserContextId::new();
//! let action = BrowserAction::Click {
//!     target: Target::new(Selector::TestId("submit-button".to_owned())),
//! };
//! let request = ActionRequest::new(context_id, action);
//! assert!(request.validate(&BrowserLimits::default()).is_ok());
//! ```

// action.rs — see CONTRACT_harw_browser.md, section `action.rs`.

use crate::diagnostic::BrowserDiagnostic;
use crate::error::{Error, Result};
use crate::ids::{BrowserContextId, BrowserObservationRevision, EffectId};
use crate::policy::BrowserLimits;
use crate::selector::{Selector, Target};

/// Maximum byte length of a `KeyPress` key name.
pub const MAX_KEY_BYTES: usize = 64;
/// Maximum absolute scroll delta per axis, in CSS pixels.
pub const MAX_SCROLL_DELTA: i32 = 100_000;

/// The closed set of mutating operations a caller may request against a
/// browser context.
///
/// # Description
/// Each variant carries exactly the data needed to perform one kind of
/// mutation: element-targeted interactions (`Click`, `Type`, `Clear`,
/// `Select`, `Focus`, `Hover`, `Drag`, `Submit`) carry one or more
/// [`Target`]s, viewport/keyboard interactions (`Scroll`, `KeyPress`) accept
/// an optional target (`None` means the action applies to the page as a
/// whole), and navigation actions (`Navigate`, `Back`, `Forward`, `Reload`)
/// carry only the data needed to navigate. There is deliberately no file
/// upload and no script execution variant. This type is serialized across
/// the tool/model boundary; unknown variants and unknown fields are rejected.
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
#[serde(deny_unknown_fields)]
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

impl BrowserAction {
    /// Validates the action's payload against `limits`.
    ///
    /// # Description
    /// Checks every target ([`validate_target`]), text/value length against
    /// [`BrowserLimits::max_text_bytes`], key names (non-empty, at most
    /// [`MAX_KEY_BYTES`], no control characters), scroll deltas (at most
    /// [`MAX_SCROLL_DELTA`] per axis) and navigation URLs (`http`/`https`
    /// only, at most [`BrowserLimits::max_url_bytes`]). It does **not** check
    /// origins; see [`crate::policy::OpenBrowserRequest::check_navigation_target`].
    ///
    /// # Arguments
    /// - `limits` (`&BrowserLimits`): the session's ceilings, borrowed.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: any limit is violated.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::action::BrowserAction;
    /// use harw_browser::policy::BrowserLimits;
    ///
    /// # fn main() -> harw_browser::Result<()> {
    /// let action = BrowserAction::Navigate { url: url::Url::parse("file:///etc/passwd")? };
    /// assert!(action.validate(&BrowserLimits::default()).is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn validate(&self, limits: &BrowserLimits) -> Result<()> {
        match self {
            Self::Click { target }
            | Self::Clear { target }
            | Self::Focus { target }
            | Self::Hover { target }
            | Self::Submit { target } => validate_target(target, limits),
            Self::Type { target, text } => {
                validate_target(target, limits)?;
                check_text_len("type text", text, limits)
            }
            Self::Select { target, value } => {
                validate_target(target, limits)?;
                check_text_len("select value", value, limits)
            }
            Self::Scroll { target, x, y } => {
                if let Some(target) = target {
                    validate_target(target, limits)?;
                }
                if x.unsigned_abs() > MAX_SCROLL_DELTA.unsigned_abs()
                    || y.unsigned_abs() > MAX_SCROLL_DELTA.unsigned_abs()
                {
                    return Err(invalid(format!(
                        "scroll delta exceeds {MAX_SCROLL_DELTA} pixels per axis"
                    )));
                }
                Ok(())
            }
            Self::KeyPress { target, key } => {
                if let Some(target) = target {
                    validate_target(target, limits)?;
                }
                if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control)
                {
                    return Err(invalid(format!(
                        "key must be 1..={MAX_KEY_BYTES} bytes without control characters"
                    )));
                }
                Ok(())
            }
            Self::Drag {
                source,
                destination,
            } => {
                validate_target(source, limits)?;
                validate_target(destination, limits)
            }
            Self::Navigate { url } => {
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(invalid(format!(
                        "navigation scheme '{}' is not allowed; only http and https",
                        url.scheme()
                    )));
                }
                limits.check_url_len(url)
            }
            Self::Back | Self::Forward | Self::Reload => Ok(()),
        }
    }

    /// Returns the explicit navigation target, if this is a `Navigate` action.
    ///
    /// # Returns
    /// `Some(&url)` for `Navigate`, otherwise `None`. Note that every other
    /// action may still navigate implicitly (links, forms, scripts); backends
    /// must check the observed location after every action.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::action::BrowserAction;
    ///
    /// assert!(BrowserAction::Reload.navigation_target().is_none());
    /// ```
    pub fn navigation_target(&self) -> Option<&url::Url> {
        match self {
            Self::Navigate { url } => Some(url),
            _ => None,
        }
    }
}

/// Validates a [`Target`]'s primary and fallback selectors against `limits`.
///
/// # Arguments
/// - `target` (`&Target`): borrowed.
/// - `limits` (`&BrowserLimits`): borrowed.
///
/// # Errors
/// - [`Error::InvalidArgument`]: more fallbacks than
///   [`BrowserLimits::max_selector_fallbacks`], or any selector invalid per
///   [`validate_selector`].
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::action::validate_target;
/// use harw_browser::policy::BrowserLimits;
/// use harw_browser::selector::{Selector, Target};
///
/// let target = Target::new(Selector::Css(String::new()));
/// assert!(validate_target(&target, &BrowserLimits::default()).is_err());
/// ```
pub fn validate_target(target: &Target, limits: &BrowserLimits) -> Result<()> {
    if target.fallbacks.len() > limits.max_selector_fallbacks() {
        return Err(invalid(format!(
            "target has {} fallback selectors, maximum is {}",
            target.fallbacks.len(),
            limits.max_selector_fallbacks()
        )));
    }
    target
        .candidates()
        .try_for_each(|selector| validate_selector(selector, limits))
}

/// Validates one [`Selector`]'s string parts against `limits`.
///
/// # Description
/// Every required string must be non-empty and at most
/// [`BrowserLimits::max_selector_bytes`]; the optional `Role::name` may be
/// empty but is length-checked. NUL bytes are rejected.
///
/// # Errors
/// - [`Error::InvalidArgument`]: a part is empty, too long, or contains NUL.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::action::validate_selector;
/// use harw_browser::policy::BrowserLimits;
/// use harw_browser::selector::Selector;
///
/// assert!(validate_selector(&Selector::Id("ok".to_owned()), &BrowserLimits::default()).is_ok());
/// ```
pub fn validate_selector(selector: &Selector, limits: &BrowserLimits) -> Result<()> {
    let check = |part: &str, allow_empty: bool| -> Result<()> {
        if (!allow_empty && part.is_empty())
            || part.len() > limits.max_selector_bytes()
            || part.contains('\0')
        {
            return Err(invalid(format!(
                "selector part must be {}..={} bytes without NUL",
                usize::from(!allow_empty),
                limits.max_selector_bytes()
            )));
        }
        Ok(())
    };
    match selector {
        Selector::TestId(value)
        | Selector::Css(value)
        | Selector::Id(value)
        | Selector::Name(value)
        | Selector::LinkText(value)
        | Selector::XPath(value)
        | Selector::TextAnchor(value) => check(value, false),
        Selector::TagClass { tag, class } => {
            check(tag, false)?;
            check(class, false)
        }
        Selector::Role { role, name } => {
            check(role, false)?;
            name.as_deref().map_or(Ok(()), |name| check(name, true))
        }
    }
}

fn check_text_len(what: &str, text: &str, limits: &BrowserLimits) -> Result<()> {
    if text.len() > limits.max_text_bytes() {
        return Err(invalid(format!(
            "{what} is {} bytes, maximum is {}",
            text.len(),
            limits.max_text_bytes()
        )));
    }
    Ok(())
}

fn invalid(detail: String) -> Error {
    Error::InvalidArgument { detail }
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
/// Unknown fields are rejected on deserialization.
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
#[serde(deny_unknown_fields)]
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

    /// Validates the contained action against `limits`.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: see [`BrowserAction::validate`].
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::action::{ActionRequest, BrowserAction};
    /// use harw_browser::ids::BrowserContextId;
    /// use harw_browser::policy::BrowserLimits;
    ///
    /// let request = ActionRequest::new(BrowserContextId::new(), BrowserAction::Back);
    /// assert!(request.validate(&BrowserLimits::default()).is_ok());
    /// ```
    pub fn validate(&self, limits: &BrowserLimits) -> Result<()> {
        self.action.validate(limits)
    }
}

/// Counts actions performed in one browser session against
/// [`BrowserLimits::max_actions_per_session`].
///
/// # Description
/// Backends or the tool layer hold one budget per session and call
/// [`ActionBudget::try_consume`] before dispatching each action. Once the
/// budget is exhausted every further attempt fails; the count never resets.
///
/// # Concurrency
/// `Send + Sync`, mutated via `&mut self`; no internal locking.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::action::ActionBudget;
/// use harw_browser::policy::BrowserLimits;
///
/// let mut budget = ActionBudget::new(&BrowserLimits::default().with_max_actions_per_session(1));
/// assert!(budget.try_consume().is_ok());
/// assert!(budget.try_consume().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionBudget {
    used: u32,
    max: u32,
}

impl ActionBudget {
    /// Creates an unused budget sized from `limits`.
    pub fn new(limits: &BrowserLimits) -> Self {
        Self {
            used: 0,
            max: limits.max_actions_per_session(),
        }
    }

    /// Consumes one action from the budget.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: the budget is exhausted; the counter is
    ///   not advanced.
    pub fn try_consume(&mut self) -> Result<()> {
        if self.used >= self.max {
            return Err(invalid(format!(
                "browser session action budget of {} actions is exhausted",
                self.max
            )));
        }
        self.used += 1;
        Ok(())
    }

    /// Returns the number of consumed actions.
    pub fn used(&self) -> u32 {
        self.used
    }

    /// Returns the number of actions still available.
    pub fn remaining(&self) -> u32 {
        self.max.saturating_sub(self.used)
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
#[serde(deny_unknown_fields)]
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
    use crate::policy::DEFAULT_MAX_TEXT_BYTES;

    fn target(id: &str) -> Target {
        Target::new(Selector::Id(id.to_owned()))
    }

    fn target_json() -> serde_json::Value {
        serde_json::to_value(target("file-input")).expect("target serializes")
    }

    #[test]
    fn test_action_request_new_has_no_expected_revision() {
        let context_id = BrowserContextId::new();
        let request = ActionRequest::new(context_id, BrowserAction::Back);
        assert_eq!(request.context_id, context_id);
        assert_eq!(request.expected_revision, None);
        assert_eq!(request.action, BrowserAction::Back);
    }

    #[test]
    fn test_action_request_with_expected_revision_sets_field() {
        let revision = BrowserObservationRevision::initial().next();
        let request = ActionRequest::new(BrowserContextId::new(), BrowserAction::Forward)
            .with_expected_revision(revision);
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
        let click_a = BrowserAction::Click {
            target: target("a"),
        };
        assert_eq!(
            click_a,
            BrowserAction::Click {
                target: target("a")
            }
        );
        assert_ne!(
            click_a,
            BrowserAction::Click {
                target: target("b")
            }
        );
        let scroll = BrowserAction::Scroll {
            target: None,
            x: 0,
            y: 100,
        };
        assert_ne!(scroll, click_a);
    }

    #[test]
    fn test_navigation_target_only_for_navigate() {
        let url = url::Url::parse("https://example.com/").expect("valid url in test");
        let navigate = BrowserAction::Navigate { url: url.clone() };
        assert_eq!(navigate.navigation_target(), Some(&url));
        assert_eq!(BrowserAction::Reload.navigation_target(), None);
    }

    #[test]
    fn test_browser_action_serde_json_round_trip() {
        let actions = vec![
            BrowserAction::Click {
                target: Target::new(Selector::Css("button.submit".to_owned())),
            },
            BrowserAction::KeyPress {
                target: None,
                key: "Enter".to_owned(),
            },
            BrowserAction::Navigate {
                url: url::Url::parse("https://example.com/page").expect("valid url in test"),
            },
            BrowserAction::Reload,
        ];
        for action in actions {
            let json = serde_json::to_string(&action).expect("action serializes");
            let decoded: BrowserAction = serde_json::from_str(&json).expect("action deserializes");
            assert_eq!(decoded, action);
        }
    }

    #[test]
    fn test_browser_action_deserialize_rejects_upload_variant() {
        for tag in ["Upload", "upload", "UploadFile", "FileUpload"] {
            let json = serde_json::json!({
                tag: { "target": target_json(), "file_path": "/home/user/.ssh/id_ed25519" }
            });
            assert!(
                serde_json::from_value::<BrowserAction>(json).is_err(),
                "variant {tag} must not be representable"
            );
        }
    }

    #[test]
    fn test_browser_action_deserialize_rejects_script_variants() {
        for tag in ["Script", "script", "CustomScript", "ExecuteScript", "Evaluate"] {
            let json = serde_json::json!({ tag: { "script": "fetch('https://evil')" } });
            assert!(serde_json::from_value::<BrowserAction>(json).is_err());
        }
    }

    #[test]
    fn test_browser_action_deserialize_rejects_unknown_fields() {
        let json = serde_json::json!({
            "Click": { "target": target_json(), "file_path": "/etc/passwd" }
        });
        assert!(serde_json::from_value::<BrowserAction>(json).is_err());
    }

    #[test]
    fn test_action_request_deserialize_rejects_unknown_fields() {
        let request = ActionRequest::new(BrowserContextId::new(), BrowserAction::Reload);
        let mut json = serde_json::to_value(&request).expect("request serializes");
        let decoded: ActionRequest =
            serde_json::from_value(json.clone()).expect("request deserializes");
        assert_eq!(decoded, request);
        json["allowed_origins"] = serde_json::json!(["https://evil.example"]);
        assert!(serde_json::from_value::<ActionRequest>(json).is_err());
    }

    #[test]
    fn test_validate_accepts_default_sized_action() {
        let action = BrowserAction::Type {
            target: target("q"),
            text: "a".repeat(DEFAULT_MAX_TEXT_BYTES),
        };
        assert!(action.validate(&BrowserLimits::default()).is_ok());
    }

    #[test]
    fn test_validate_rejects_text_over_limit() {
        let limits = BrowserLimits::default().with_max_text_bytes(8);
        let typed = BrowserAction::Type {
            target: target("q"),
            text: "123456789".to_owned(),
        };
        assert!(typed.validate(&limits).is_err());
        let selected = BrowserAction::Select {
            target: target("q"),
            value: "123456789".to_owned(),
        };
        assert!(selected.validate(&limits).is_err());
    }

    #[test]
    fn test_validate_rejects_selector_over_limit_or_empty() {
        let limits = BrowserLimits::default().with_max_selector_bytes(4);
        let long = BrowserAction::Click {
            target: Target::new(Selector::XPath("//div".to_owned())),
        };
        assert!(long.validate(&limits).is_err());
        let empty = BrowserAction::Hover {
            target: Target::new(Selector::Css(String::new())),
        };
        assert!(empty.validate(&BrowserLimits::default()).is_err());
        let fallback_too_long = BrowserAction::Drag {
            source: target("a"),
            destination: target("b").with_fallback(Selector::Name("toolong".to_owned())),
        };
        assert!(fallback_too_long.validate(&limits).is_err());
    }

    #[test]
    fn test_validate_rejects_too_many_fallbacks() {
        let limits = BrowserLimits::default().with_max_selector_fallbacks(1);
        let target = target("a")
            .with_fallback(Selector::Id("b".to_owned()))
            .with_fallback(Selector::Id("c".to_owned()));
        let action = BrowserAction::Submit { target };
        assert!(action.validate(&limits).is_err());
    }

    #[test]
    fn test_validate_role_selector_optional_name() {
        let limits = BrowserLimits::default();
        let with_empty_name = Selector::Role {
            role: "button".to_owned(),
            name: Some(String::new()),
        };
        assert!(validate_selector(&with_empty_name, &limits).is_ok());
        let nul = Selector::TagClass {
            tag: "div".to_owned(),
            class: "a\0b".to_owned(),
        };
        assert!(validate_selector(&nul, &limits).is_err());
    }

    #[test]
    fn test_validate_rejects_bad_key_and_scroll() {
        let limits = BrowserLimits::default();
        for key in [String::new(), "a".repeat(MAX_KEY_BYTES + 1), "\u{7}".to_owned()] {
            let action = BrowserAction::KeyPress { target: None, key };
            assert!(action.validate(&limits).is_err());
        }
        let scroll = BrowserAction::Scroll {
            target: None,
            x: i32::MIN,
            y: 0,
        };
        assert!(scroll.validate(&limits).is_err());
        let ok = BrowserAction::Scroll {
            target: None,
            x: -MAX_SCROLL_DELTA,
            y: MAX_SCROLL_DELTA,
        };
        assert!(ok.validate(&limits).is_ok());
    }

    #[test]
    fn test_validate_navigate_scheme_and_length() {
        let limits = BrowserLimits::default().with_max_url_bytes(40);
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>1</script>",
            "https://example.com/this/path/is/definitely/too/long",
        ] {
            let action = BrowserAction::Navigate {
                url: url::Url::parse(bad).expect("valid url in test"),
            };
            assert!(action.validate(&limits).is_err(), "{bad} must be rejected");
        }
        let ok = BrowserAction::Navigate {
            url: url::Url::parse("https://example.com/").expect("valid url in test"),
        };
        assert!(ok.validate(&limits).is_ok());
    }

    #[test]
    fn test_action_budget_try_consume_until_exhausted() {
        let mut budget =
            ActionBudget::new(&BrowserLimits::default().with_max_actions_per_session(2));
        assert_eq!(budget.remaining(), 2);
        assert!(budget.try_consume().is_ok());
        assert!(budget.try_consume().is_ok());
        assert!(budget.try_consume().is_err());
        assert_eq!(budget.used(), 2);
        assert_eq!(budget.remaining(), 0);
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
