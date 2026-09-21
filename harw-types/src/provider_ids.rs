//! # Provider and Model Identifiers
//!
//! ## Responsibility
//!
//! This module owns the six `Arc<str>`-backed newtype identifiers used to name
//! providers, models, agents, and customers throughout the harness. It does **not**
//! own session- or turn-level identifiers (those live in [`crate::ids`]), nor does
//! it perform any provider I/O or registry look-ups — it is a pure-data vocabulary
//! layer with no business logic.
//!
//! ## Key types exported
//!
//! - [`ProviderId`] — stable, machine-readable provider key (e.g. `"openai"`).
//! - [`ProviderName`] — human-readable or look-up name of a provider.
//! - [`ModelId`] — stable, machine-readable model key (e.g. `"gpt-5"`).
//! - [`ModelName`] — human-readable or look-up name of a model.
//! - [`AgentName`] — agent identifier used in provider-override routing.
//! - [`CustomerId`] — tenant/customer designator for per-customer provider settings.
//!
//! Every type is produced by the internal `arc_str_id!` macro and therefore carries
//! an identical set of trait implementations — see the *Ergonomic impls* section below.
//!
//! ## Why `Arc<str>` instead of `String`
//!
//! These identifiers appear in registries, fallback chains, and agent-override maps
//! where the same value is held by many owners simultaneously. `Arc<str>` makes
//! every [`Clone`] a single atomic reference-count increment rather than a heap
//! allocation and `memcpy`. The inner slice is immutable, so no synchronisation
//! primitive is required on the data itself.
//!
//! ## Ergonomic impls (applied to all six newtypes by `arc_str_id!`)
//!
//! | Trait | Effect |
//! |---|---|
//! | `Deref<Target = str>` | `&id` coerces to `&str`; any `&str` API accepts `&id` directly. |
//! | `AsRef<str>` | Satisfies generic bounds like `T: AsRef<str>`. |
//! | `Borrow<str>` | Enables `HashMap<ProviderId, V>::get("key")` without an owned key. |
//! | `PartialEq<str>`, `PartialEq<&str>`, `PartialEq<String>` | `id == "openai"` compiles. |
//! | `PartialEq<ProviderId> for str/&str/String` | `"openai" == id` compiles (reflexive). |
//! | `Ord` + `PartialOrd` | Lexicographic ordering; IDs can be sorted or used in `BTreeMap`. |
//! | `Serialize` / `Deserialize` | Wire-transparent: serialises as a bare JSON string, no wrapper object; deserialisation rejects blank values. |
//! | `From<&str>` / `From<String>` | Cheap infallible construction from any string source. |
//! | `try_from_str` / `parse` / `FromStr` | Fallible construction for untrusted input; rejects empty and whitespace-only values with [`InvalidId`]. |
//! | `Display` | Formats as the raw string value. |
//!
//! ## Concurrency
//!
//! All six types are `Send + Sync` because `Arc<str>` is `Send + Sync`. Cloning
//! an identifier is thread-safe (atomic reference count). No `Mutex` or other
//! synchronisation is involved. The types contain no interior mutability.
//!
//! ## Errors
//!
//! Infallible construction via [`ProviderId::new`] (and the equivalent on all
//! other types), `From<&str>`, and `From<String>` remains available for backwards
//! compatibility. Untrusted input must use `try_from_str`, `parse`, or
//! [`std::str::FromStr::from_str`], all of which reject empty and whitespace-only
//! values with [`InvalidId`]. Deserialisation uses the same validation and reports
//! failures through the deserialiser's error type.
//!
//! ## Examples
//!
//! ```rust,no_run
//! use harw_types::{ProviderId, ModelId};
//! use std::collections::HashMap;
//!
//! // Construction — all three forms are equivalent.
//! let pid = ProviderId::from("openai");
//! let mid = ModelId::new("gpt-5");
//!
//! // Deref coercion: pass directly to any `&str` API.
//! fn accepts_str(s: &str) { let _ = s.len(); }
//! accepts_str(&pid);
//!
//! // Comparison without `.as_str()`.
//! assert_eq!(pid, "openai");
//! assert_eq!("openai", pid);
//!
//! // HashMap look-up by `&str` thanks to `Borrow<str>`.
//! let mut registry: HashMap<ProviderId, u32> = HashMap::new();
//! registry.insert(pid, 1);
//! assert_eq!(registry.get::<str>("openai"), Some(&1));
//!
//! // Sorting via `Ord`.
//! let mut ids = vec![ModelId::from("gpt-5"), ModelId::from("claude-4")];
//! ids.sort();
//! assert_eq!(ids[0], "claude-4");
//! ```

pub use crate::error::InvalidId;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

fn validate_id(id_type: &'static str, value: impl Into<Arc<str>>) -> Result<Arc<str>, InvalidId> {
    let value = value.into();
    if value.trim().is_empty() {
        Err(InvalidId::new(id_type))
    } else {
        Ok(value)
    }
}

/// Declares an `Arc<str>`-backed identifier newtype with a full ergonomic impl suite.
///
/// # Description
///
/// Each invocation of `arc_str_id!(TypeName)` expands into a complete
/// `pub struct TypeName(pub Arc<str>)` together with the following trait
/// implementations:
///
/// - [`Clone`], [`Debug`](std::fmt::Debug), [`PartialEq`], [`Eq`], [`Hash`] — derived.
/// - [`Serialize`](serde::Serialize) / [`Deserialize`](serde::Deserialize) —
///   wire-transparent: the type serialises as a bare JSON string, never as an object
///   wrapper; deserialisation rejects empty and whitespace-only values.
/// - [`From<&str>`](From) / [`From<String>`](From) — infallible construction from any
///   string source.
/// - `try_from_str`, `parse`, and [`FromStr`] — validated construction for untrusted
///   input, returning [`InvalidId`] for empty and whitespace-only values.
/// - [`fmt::Display`] — formats as the raw inner string.
/// - [`std::ops::Deref`]`<Target = str>` — `&id` coerces to `&str`.
/// - [`AsRef<str>`] — satisfies generic `T: AsRef<str>` bounds.
/// - [`std::borrow::Borrow<str>`] — enables `HashMap<T, V>::get(&str)` look-ups.
/// - Bidirectional [`PartialEq`] with `str`, `&str`, and [`String`] — so
///   `id == "value"` and `"value" == id` both compile without `.as_str()`.
/// - [`PartialOrd`] + [`Ord`] — lexicographic ordering over the inner string slice.
///
/// # Concurrency
///
/// The generated type is `Send + Sync` because `Arc<str>` is `Send + Sync`. Cloning
/// costs one atomic increment. There is no interior mutability.
macro_rules! arc_str_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(pub Arc<str>);

        /// Serialises as a bare JSON string (wire-transparent, no object wrapper).
        ///
        /// # Concurrency
        ///
        /// Stateless — safe to call from any thread.
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        /// Deserialises a non-blank bare JSON string into an `Arc<str>`-backed newtype.
        ///
        /// # Errors
        ///
        /// Returns `D::Error` when the input is not a valid, non-blank string according
        /// to the deserialiser (e.g. a JSON number, object, empty string, or
        /// whitespace-only string).
        ///
        /// # Concurrency
        ///
        /// Stateless — safe to call from any thread.
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                Self::try_from_str(s).map_err(serde::de::Error::custom)
            }
        }

        impl $name {
            /// Constructs the identifier from any value that converts into `Arc<str>`.
            ///
            /// # Description
            ///
            /// Accepts `&str`, `String`, `Arc<str>`, or any other type implementing
            /// `Into<Arc<str>>`. When the source is already an `Arc<str>`, no heap
            /// allocation occurs. For `&str` or `String` inputs, exactly one
            /// allocation is made to place the bytes into the `Arc`.
            ///
            /// # Arguments
            ///
            /// - `value` (`impl Into<Arc<str>>`): the string content for the identifier.
            ///   Ownership is transferred into the `Arc`.
            ///
            /// # Returns
            ///
            /// A new `Self` wrapping the string in a reference-counted allocation.
            ///
            /// # Concurrency
            ///
            /// Safe to call from any thread; `Arc::from` is thread-safe.
            ///
            /// # Examples
            ///
            /// ```rust,no_run
            /// # use harw_types::ProviderId;
            /// let id = ProviderId::new("openai");
            /// assert_eq!(id.as_str(), "openai");
            /// ```
            pub fn new(value: impl Into<Arc<str>>) -> Self {
                Self(value.into())
            }

            /// Constructs an identifier from a non-blank value.
            ///
            /// Use this constructor for strings obtained from configuration, user
            /// input, or a wire format. It preserves non-blank surrounding
            /// whitespace, matching the canonical ID validation convention.
            ///
            /// # Errors
            ///
            /// Returns [`InvalidId`] when `value` is empty or whitespace-only.
            pub fn try_from_str(value: impl Into<Arc<str>>) -> Result<Self, InvalidId> {
                validate_id(stringify!($name), value).map(Self)
            }

            /// Alias for [`Self::try_from_str`] for parser-oriented call sites.
            ///
            /// # Errors
            ///
            /// Returns [`InvalidId`] when `value` is empty or whitespace-only.
            pub fn parse(value: impl Into<Arc<str>>) -> Result<Self, InvalidId> {
                Self::try_from_str(value)
            }

            /// Borrows the inner string slice.
            ///
            /// # Description
            ///
            /// Returns a `&str` bound to the lifetime of `self`. Equivalent to
            /// `&*self` via [`Deref`](std::ops::Deref), but explicit when the coercion
            /// is not inferred by the compiler.
            ///
            /// # Returns
            ///
            /// `&str` — a borrowed view of the identifier's string content. The
            /// slice is valid as long as `self` (or any other `Arc` clone) is live.
            ///
            /// # Concurrency
            ///
            /// Read-only; safe to call from any thread.
            ///
            /// # Examples
            ///
            /// ```rust,no_run
            /// # use harw_types::ModelId;
            /// let id = ModelId::from("gpt-5");
            /// assert_eq!(id.as_str(), "gpt-5");
            /// ```
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        /// Formats the identifier as its raw string value (no surrounding quotes or
        /// type name).
        ///
        /// # Concurrency
        ///
        /// Stateless — safe to call from any thread.
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        /// Constructs the identifier from a borrowed string slice.
        ///
        /// # Description
        ///
        /// Allocates a new `Arc<str>` containing a copy of `value`. This is the
        /// cheapest construction path when the caller already holds a `&str` (e.g.
        /// a string literal), as no intermediate `String` is created.
        ///
        /// # Arguments
        ///
        /// - `value` (`&str`): the string content. The bytes are copied into the `Arc`.
        ///
        /// # Returns
        ///
        /// A new `Self` wrapping the copied bytes in a reference-counted allocation.
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(Arc::from(value))
            }
        }

        /// Constructs the identifier from an owned `String`.
        ///
        /// # Description
        ///
        /// Converts `value` into an `Arc<str>`. The owned `String`'s heap buffer is
        /// not reused — `Arc::from(&str)` performs a copy into a new, exactly-sized
        /// `Arc` allocation. The original `String` is dropped after the conversion.
        ///
        /// # Arguments
        ///
        /// - `value` (`String`): the owned string. Consumed by the conversion.
        ///
        /// # Returns
        ///
        /// A new `Self` backed by a freshly allocated `Arc<str>`.
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(Arc::from(value.as_str()))
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_from_str(value)
            }
        }

        // ── Deref / AsRef / Borrow ──────────────────────────────────────────

        /// Dereferences to `str`, enabling `&id` to coerce to `&str`.
        ///
        /// # Description
        ///
        /// Because this impl is present, a `&TypeName` is accepted wherever a
        /// `&str` is expected — including function calls, slice indexing, and
        /// string method dispatch — without an explicit `.as_str()` call.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl std::ops::Deref for $name {
            type Target = str;

            fn deref(&self) -> &str {
                self.as_str()
            }
        }

        /// Returns a `&str` view of the identifier, satisfying `T: AsRef<str>` bounds.
        ///
        /// # Description
        ///
        /// Generic APIs parameterised on `T: AsRef<str>` (e.g. `Path::new`,
        /// `HashMap::get`, custom APIs) can accept a `&TypeName` directly without
        /// the caller converting to `&str` first.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        /// Borrows the identifier as `str`, enabling `HashMap<TypeName, V>::get(&str)`.
        ///
        /// # Description
        ///
        /// [`std::collections::HashMap`] and [`std::collections::BTreeMap`] use the
        /// [`std::borrow::Borrow`] trait to allow look-ups with a borrowed form of
        /// the key. Because `TypeName: Borrow<str>`, a `HashMap<TypeName, V>` can be
        /// queried with a plain `&str` reference — no owned key needs to be
        /// constructed just for the look-up.
        ///
        /// The `Borrow` contract requires that [`Hash`](std::hash::Hash) and [`Eq`]
        /// of the borrowed form (`str`) agree with those of the owned form
        /// (`TypeName`). This holds because both delegate to the same byte sequence.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }

        // ── PartialEq — newtype side ────────────────────────────────────────

        /// Compares the identifier's string content against an unowned `str` slice.
        ///
        /// # Description
        ///
        /// Enables `id == "some_value"` without an `.as_str()` call on the
        /// left-hand side. Delegates to byte-equality of the inner slice.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }

        /// Compares the identifier's string content against a `&str` reference.
        ///
        /// # Description
        ///
        /// Enables `id == some_str_ref` where the right-hand side is a `&&str`
        /// (a reference to a string slice reference), which arises in generic
        /// contexts and iterator adapters. Delegates to byte-equality.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        /// Compares the identifier's string content against an owned `String`.
        ///
        /// # Description
        ///
        /// Enables `id == some_string` without calling `.as_str()` on either side.
        /// Delegates to byte-equality of the underlying string slices.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                self.as_str() == other.as_str()
            }
        }

        // ── PartialEq — reflexive (str / &str / String on the left) ────────

        /// Compares a `str` slice against the identifier (reflexive direction).
        ///
        /// # Description
        ///
        /// Enables `"some_value" == id` — the symmetric counterpart to
        /// `PartialEq<str> for TypeName`. Without this impl the compiler cannot
        /// infer the comparison when the string literal is on the left.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl PartialEq<$name> for str {
            fn eq(&self, other: &$name) -> bool {
                self == other.as_str()
            }
        }

        /// Compares a `&str` reference against the identifier (reflexive direction).
        ///
        /// # Description
        ///
        /// Enables `some_str_ref == id` in contexts where the left-hand side is a
        /// `&str` variable. The symmetric counterpart to `PartialEq<&str> for TypeName`.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl PartialEq<$name> for &str {
            fn eq(&self, other: &$name) -> bool {
                *self == other.as_str()
            }
        }

        /// Compares an owned `String` against the identifier (reflexive direction).
        ///
        /// # Description
        ///
        /// Enables `some_string == id` without converting the `String` to `&str`
        /// first. The symmetric counterpart to `PartialEq<String> for TypeName`.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl PartialEq<$name> for String {
            fn eq(&self, other: &$name) -> bool {
                self.as_str() == other.as_str()
            }
        }

        // ── Ord / PartialOrd ────────────────────────────────────────────────

        /// Returns a partial ordering between two identifiers.
        ///
        /// # Description
        ///
        /// Delegates to [`Ord::cmp`], so the result is always `Some`. Lexicographic
        /// ordering matches the ordering of the underlying string bytes, which is
        /// consistent with [`Hash`](std::hash::Hash) and [`Eq`] as required by the
        /// standard library.
        ///
        /// # Returns
        ///
        /// `Some(`[`Ordering`]`)` — never `None`; the type has a total order.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        /// Returns the total lexicographic ordering between two identifiers.
        ///
        /// # Description
        ///
        /// Compares the inner `str` slices byte-by-byte in the manner of
        /// [`str::cmp`]. This ordering is stable and deterministic — equal strings
        /// compare as [`Ordering::Equal`], ASCII uppercase precedes lowercase, and
        /// so on. IDs may therefore be stored in [`std::collections::BTreeMap`],
        /// used as [`std::collections::BTreeSet`] elements, or sorted with
        /// [`slice::sort`](slice::sort).
        ///
        /// # Returns
        ///
        /// [`Ordering`] — `Less`, `Equal`, or `Greater`.
        ///
        /// # Concurrency
        ///
        /// Read-only; safe to call from any thread.
        impl Ord for $name {
            fn cmp(&self, other: &Self) -> Ordering {
                self.as_str().cmp(other.as_str())
            }
        }
    };
}

arc_str_id!(
    /// Stable, machine-readable provider identifier (e.g. `"openai"`, `"anthropic"`).
    ///
    /// # Description
    ///
    /// `ProviderId` is the canonical key used to look up a provider in registries
    /// and routing tables. It must be unique per provider and must not change between
    /// deployments — it is a machine key, not a display name. For the human-readable
    /// label use [`ProviderName`].
    ///
    /// All ergonomic impls described in the [`provider_ids`](crate::provider_ids)
    /// module docs apply: `Deref<Target = str>`, `AsRef<str>`, `Borrow<str>`,
    /// bidirectional `PartialEq` with `str`/`&str`/`String`, and `Ord`.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use harw_types::ProviderId;
    ///
    /// let id = ProviderId::from("openai");
    /// assert_eq!(id, "openai");
    /// assert_eq!("openai", id);
    /// ```
    ProviderId
);

arc_str_id!(
    /// Human-readable or look-up name of a provider (e.g. `"OpenAI"`, `"Anthropic"`).
    ///
    /// # Description
    ///
    /// `ProviderName` is the display or configuration-facing label for a provider.
    /// It may be less stable than [`ProviderId`] and is primarily used for logging,
    /// UI rendering, and config file keys that prefer natural names over opaque
    /// identifiers.
    ///
    /// All ergonomic impls described in the [`provider_ids`](crate::provider_ids)
    /// module docs apply.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use harw_types::ProviderName;
    ///
    /// let name = ProviderName::new("OpenAI");
    /// println!("{name}"); // prints: OpenAI
    /// ```
    ProviderName
);

arc_str_id!(
    /// Stable, machine-readable model identifier (e.g. `"gpt-5"`, `"claude-opus-4"`).
    ///
    /// # Description
    ///
    /// `ModelId` is the canonical key used to select a model in provider requests
    /// and capability registries. It must match the identifier expected by the
    /// provider's API. For the human-readable label use [`ModelName`].
    ///
    /// Because `ModelId` implements [`Ord`], a collection of model IDs can be sorted
    /// or stored in a `BTreeMap` without an extra comparator.
    ///
    /// All ergonomic impls described in the [`provider_ids`](crate::provider_ids)
    /// module docs apply.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use harw_types::ModelId;
    ///
    /// let mut ids = vec![ModelId::from("gpt-5"), ModelId::from("claude-opus-4")];
    /// ids.sort();
    /// assert_eq!(ids[0], "claude-opus-4"); // lexicographic order
    /// ```
    ModelId
);

arc_str_id!(
    /// Human-readable or look-up name of a model (e.g. `"GPT-5"`, `"Claude Opus 4"`).
    ///
    /// # Description
    ///
    /// `ModelName` is the display or config-facing label for a model. It is separate
    /// from [`ModelId`] so that display names can be updated without touching the
    /// stable API key.
    ///
    /// All ergonomic impls described in the [`provider_ids`](crate::provider_ids)
    /// module docs apply.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use harw_types::ModelName;
    ///
    /// let name = ModelName::new("Claude Opus 4");
    /// assert_eq!(name.as_str(), "Claude Opus 4");
    /// ```
    ModelName
);

arc_str_id!(
    /// Agent identifier used in provider-override routing.
    ///
    /// # Description
    ///
    /// `AgentName` identifies a named agent whose requests may be routed to a
    /// specific provider or model, overriding the default selection. It is consumed
    /// by the provider registry when resolving which backend to call for a given turn.
    /// Unlike session/turn IDs (see [`crate::ids`]), agent names are stable,
    /// configuration-sourced strings rather than randomly generated UUIDs.
    ///
    /// All ergonomic impls described in the [`provider_ids`](crate::provider_ids)
    /// module docs apply.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use harw_types::AgentName;
    ///
    /// let agent = AgentName::from("summariser");
    /// assert_eq!(agent, "summariser");
    /// ```
    AgentName
);

arc_str_id!(
    /// Tenant or customer designator for per-customer provider settings.
    ///
    /// # Description
    ///
    /// `CustomerId` scopes provider configuration (API keys, rate limits, model
    /// preferences) to a specific customer or tenant. It is an opaque string rather
    /// than a structured type so that the harness remains agnostic to the caller's
    /// identity-management scheme (UUID, slug, account number, etc.).
    ///
    /// All ergonomic impls described in the [`provider_ids`](crate::provider_ids)
    /// module docs apply.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use harw_types::CustomerId;
    ///
    /// let cid = CustomerId::new("acme-corp");
    /// assert_eq!(cid.as_str(), "acme-corp");
    /// ```
    CustomerId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    // ── helper ──────────────────────────────────────────────────────────────

    /// Accepts any `&str`-deref-able value and returns its length.
    fn str_len(s: &str) -> usize {
        s.len()
    }

    macro_rules! assert_fallible_apis_reject_blank_ids {
        ($id_type:ty) => {
            assert!(<$id_type>::try_from_str("").is_err());
            assert!(<$id_type>::parse(" \t\n").is_err());
            assert!("".parse::<$id_type>().is_err());
        };
    }

    #[test]
    fn every_identifier_type_rejects_blank_values_from_fallible_apis() {
        assert_fallible_apis_reject_blank_ids!(ProviderId);
        assert_fallible_apis_reject_blank_ids!(ProviderName);
        assert_fallible_apis_reject_blank_ids!(ModelId);
        assert_fallible_apis_reject_blank_ids!(ModelName);
        assert_fallible_apis_reject_blank_ids!(AgentName);
        assert_fallible_apis_reject_blank_ids!(CustomerId);

        assert_eq!(
            ProviderId::try_from_str(" openai ").unwrap().as_str(),
            " openai "
        );
    }

    #[test]
    fn deserialization_rejects_blank_identifier_values() {
        assert!(serde_json::from_str::<ProviderId>(r#"""#).is_err());
        assert!(serde_json::from_str::<ModelId>(r#""  \t\n""#).is_err());
        assert_eq!(
            serde_json::from_str::<ProviderName>(r#""OpenAI""#)
                .unwrap()
                .as_str(),
            "OpenAI"
        );
    }

    #[test]
    fn legacy_infallible_construction_remains_available() {
        assert!(ProviderId::new("").as_str().is_empty());
        assert!(ModelId::from(" \t\n").as_str().trim().is_empty());
    }

    // ── ProviderId (existing) ────────────────────────────────────────────────

    /// Verifies Deref coercion: `&ProviderId` can be passed as `&str`.
    #[test]
    fn test_provider_id_deref_coercion() {
        let id = ProviderId::from("openai");
        assert_eq!(str_len(&id), 6);
    }

    /// Verifies bidirectional PartialEq between ProviderId and &str / str.
    #[test]
    fn test_provider_id_eq_str() {
        let id = ProviderId::from("openai");
        assert_eq!(id, "openai");
        assert_eq!("openai", id);
        assert_eq!(id, "openai".to_owned());
        assert_eq!("openai".to_owned(), id);
    }

    /// Verifies that ModelId implements Ord and Vec::sort produces lexicographic order.
    #[test]
    fn test_model_id_orderable() {
        let mut ids = vec![
            ModelId::from("gpt-5"),
            ModelId::from("claude-4"),
            ModelId::from("gemini-2"),
        ];
        ids.sort();
        assert_eq!(
            ids,
            vec![
                ModelId::from("claude-4"),
                ModelId::from("gemini-2"),
                ModelId::from("gpt-5"),
            ]
        );
    }

    /// Verifies Borrow<str>: a HashMap<ProviderId, u32> can be looked up with `&str`.
    #[test]
    fn test_provider_id_as_hashmap_key_borrow_str() {
        let mut map: HashMap<ProviderId, u32> = HashMap::new();
        map.insert(ProviderId::from("openai"), 42);
        let result = map.get::<str>("openai");
        assert_eq!(result, Some(&42));
    }

    // ── ProviderId – BTreeMap key (Ord proves map ordering) ─────────────────

    /// Verifies that `ProviderId` works as a `BTreeMap` key and that iteration
    /// yields entries in ascending lexicographic order, proving `Ord` is sound.
    #[test]
    fn test_provider_id_btreemap_key_ordered() {
        let mut map: BTreeMap<ProviderId, u32> = BTreeMap::new();
        map.insert(ProviderId::from("openai"), 1);
        map.insert(ProviderId::from("anthropic"), 2);
        map.insert(ProviderId::from("google"), 3);

        let keys: Vec<&ProviderId> = map.keys().collect();
        assert_eq!(keys[0].as_str(), "anthropic");
        assert_eq!(keys[1].as_str(), "google");
        assert_eq!(keys[2].as_str(), "openai");

        // `&str` look-up via Borrow<str>
        assert_eq!(map.get::<str>("google"), Some(&3));
    }

    // ── ProviderName ─────────────────────────────────────────────────────────

    /// Verifies Deref coercion for `ProviderName`.
    #[test]
    fn test_provider_name_deref_coercion() {
        let name = ProviderName::from("OpenAI");
        assert_eq!(str_len(&name), 6);
    }

    /// Verifies bidirectional PartialEq for `ProviderName` with `&str` and `String`.
    #[test]
    fn test_provider_name_eq_str_bidirectional() {
        let name = ProviderName::from("OpenAI");
        assert_eq!(name, "OpenAI");
        assert_eq!("OpenAI", name);
        assert_eq!(name, "OpenAI".to_owned());
        assert_eq!("OpenAI".to_owned(), name);
    }

    /// Verifies that a `[ProviderName]` sorts lexicographically via `Ord`.
    #[test]
    fn test_provider_name_sort_ord() {
        let mut names = [
            ProviderName::from("OpenAI"),
            ProviderName::from("Anthropic"),
            ProviderName::from("Google"),
        ];
        names.sort();
        assert_eq!(names[0], "Anthropic");
        assert_eq!(names[1], "Google");
        assert_eq!(names[2], "OpenAI");
    }

    // ── ModelName ────────────────────────────────────────────────────────────

    /// Verifies Deref coercion for `ModelName`.
    #[test]
    fn test_model_name_deref_coercion() {
        let name = ModelName::from("GPT-5");
        assert_eq!(str_len(&name), 5);
    }

    /// Verifies bidirectional PartialEq for `ModelName` with `&str` and `String`.
    #[test]
    fn test_model_name_eq_str_bidirectional() {
        let name = ModelName::from("GPT-5");
        assert_eq!(name, "GPT-5");
        assert_eq!("GPT-5", name);
        assert_eq!(name, "GPT-5".to_owned());
        assert_eq!("GPT-5".to_owned(), name);
    }

    /// Verifies that a `[ModelName]` sorts lexicographically via `Ord`.
    /// ASCII order: 'C' (67) < 'G' (71), and "GPT-5" < "Gemini-2" because 'P' < 'e'.
    #[test]
    fn test_model_name_sort_ord() {
        let mut names = [
            ModelName::from("GPT-5"),
            ModelName::from("Claude-4"),
            ModelName::from("Gemini-2"),
        ];
        names.sort();
        assert_eq!(names[0], "Claude-4");
        assert_eq!(names[1], "GPT-5");
        assert_eq!(names[2], "Gemini-2");
    }

    // ── AgentName ────────────────────────────────────────────────────────────

    /// Verifies Deref coercion for `AgentName`.
    #[test]
    fn test_agent_name_deref_coercion() {
        let name = AgentName::from("summariser");
        assert_eq!(str_len(&name), 10);
    }

    /// Verifies bidirectional PartialEq for `AgentName` with `&str` and `String`.
    #[test]
    fn test_agent_name_eq_str_bidirectional() {
        let name = AgentName::from("summariser");
        assert_eq!(name, "summariser");
        assert_eq!("summariser", name);
        assert_eq!(name, "summariser".to_owned());
        assert_eq!("summariser".to_owned(), name);
    }

    /// Verifies that a `[AgentName]` sorts lexicographically via `Ord`.
    #[test]
    fn test_agent_name_sort_ord() {
        let mut names = [
            AgentName::from("translator"),
            AgentName::from("classifier"),
            AgentName::from("summariser"),
        ];
        names.sort();
        assert_eq!(names[0], "classifier");
        assert_eq!(names[1], "summariser");
        assert_eq!(names[2], "translator");
    }

    // ── CustomerId ───────────────────────────────────────────────────────────

    /// Verifies Deref coercion for `CustomerId`.
    #[test]
    fn test_customer_id_deref_coercion() {
        let id = CustomerId::from("acme-corp");
        assert_eq!(str_len(&id), 9);
    }

    /// Verifies bidirectional PartialEq for `CustomerId` with `&str` and `String`.
    #[test]
    fn test_customer_id_eq_str_bidirectional() {
        let id = CustomerId::from("acme-corp");
        assert_eq!(id, "acme-corp");
        assert_eq!("acme-corp", id);
        assert_eq!(id, "acme-corp".to_owned());
        assert_eq!("acme-corp".to_owned(), id);
    }

    /// Verifies that a `[CustomerId]` sorts lexicographically via `Ord`.
    #[test]
    fn test_customer_id_sort_ord() {
        let mut ids = [
            CustomerId::from("zeta-inc"),
            CustomerId::from("acme-corp"),
            CustomerId::from("beta-ltd"),
        ];
        ids.sort();
        assert_eq!(ids[0], "acme-corp");
        assert_eq!(ids[1], "beta-ltd");
        assert_eq!(ids[2], "zeta-inc");
    }
}
