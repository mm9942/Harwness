//! `limits_struct!`: declarative bounds structs with named default constants.
//!
//! Many crates hold a small `Copy` struct of numeric/`Duration` bounds plus a
//! hand-written `impl Default` that repeats each default as an inline literal.
//! [`limits_struct!`](crate::limits_struct) declares the struct, its
//! `Default`, one `pub const` per field and an associated `Self::DEFAULT`
//! (usable in `const` contexts and in other `limits_struct!` defaults) in one
//! place. Doc comments and attributes on the struct and on its fields are
//! forwarded unchanged.
//!
//! ```
//! use std::time::Duration;
//!
//! harw_types::limits_struct! {
//!     /// Bounds of one queue.
//!     #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//!     pub struct QueueBounds {
//!         /// Most queued items.
//!         pub max_items: usize = DEFAULT_MAX_ITEMS = 8,
//!         /// Time an item may wait.
//!         pub max_wait: Duration = DEFAULT_MAX_WAIT = Duration::from_secs(5),
//!     }
//! }
//!
//! assert_eq!(QueueBounds::default().max_items, DEFAULT_MAX_ITEMS);
//! assert_eq!(QueueBounds::DEFAULT, QueueBounds::default());
//! ```

/// Declares a limits struct with `Default`, `DEFAULT_*` constants and
/// `Self::DEFAULT`.
///
/// Each field is written `vis name: Type = CONST_NAME = const_expr,`. The
/// macro emits, in the calling module, `<struct vis> const CONST_NAME: Type =
/// const_expr;` per field, plus `impl Struct { pub const DEFAULT: Self }` and
/// `impl Default for Struct` returning it. The default expressions must be
/// `const`-evaluable. Derives and other attributes are written on the struct as
/// usual.
#[macro_export]
macro_rules! limits_struct {
    (
        $(#[$smeta:meta])*
        $svis:vis struct $name:ident {
            $(
                $(#[$fmeta:meta])*
                $fvis:vis $field:ident : $ty:ty = $cname:ident = $default:expr
            ),+ $(,)?
        }
    ) => {
        $(#[$smeta])*
        $svis struct $name {
            $(
                $(#[$fmeta])*
                $fvis $field: $ty,
            )+
        }

        $(
            #[doc = concat!("Default of [`", stringify!($name), "::", stringify!($field), "`].")]
            $svis const $cname: $ty = $default;
        )+

        impl $name {
            /// All-defaults value, usable in `const` contexts.
            $svis const DEFAULT: Self = Self { $($field: $cname),+ };
        }

        impl ::core::default::Default for $name {
            fn default() -> Self {
                Self::DEFAULT
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    limits_struct! {
        /// Test bounds.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct Bounds {
            /// Item cap.
            pub cap: usize = DEFAULT_CAP = 8,
            /// Wait time.
            pub wait: Duration = DEFAULT_WAIT = Duration::from_secs(3),
        }
    }

    #[test]
    fn default_matches_constants() {
        let bounds = Bounds::default();
        assert_eq!(bounds.cap, DEFAULT_CAP);
        assert_eq!(bounds.wait, DEFAULT_WAIT);
        assert_eq!(bounds, Bounds::DEFAULT);
    }

    #[test]
    fn struct_update_syntax_keeps_other_defaults() {
        let bounds = Bounds {
            cap: 1,
            ..Bounds::default()
        };
        assert_eq!(bounds.cap, 1);
        assert_eq!(bounds.wait, DEFAULT_WAIT);
    }
}
