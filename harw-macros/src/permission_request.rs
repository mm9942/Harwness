//! `permission_request!` expansion and the one permission-name table.
//!
//! [`PERMISSION_VALUES`] is the only place in this crate that maps a
//! permission spelling (`read_workspace`) onto a variant of
//! `harw_authority::Permission` (`ReadWorkspace`). Both `#[tool(permission =
//! "...")]` ([`crate::tool`]) and `permission_request!` resolve through it,
//! so a typo can never resolve to a *different* (possibly weaker) permission
//! in one macro and be rejected in the other.
//!
//! # `permission_request!` (authority doc §6.3)
//!
//! ```ignore
//! let request = harw_macros::permission_request!(read_workspace, write_workspace);
//! ```
//!
//! expands to
//!
//! ```ignore
//! ::harw_authority::PermissionRequest::from_permissions([
//!     ::harw_authority::Permission::ReadWorkspace,
//!     ::harw_authority::Permission::WriteWorkspace,
//! ])
//! ```
//!
//! and an empty list to `::harw_authority::PermissionRequest::empty()`.
//!
//! The expansion only ever calls the public *request* API of
//! `harw-authority`. A `PermissionRequest` carries no authority of its own;
//! it becomes a grant only when `PolicyBootstrap` evaluates it or an
//! existing `AuthorityContext` restricts itself with it. The macro offers no
//! syntax that builds a `PermissionSet`, an `AuthorityContext` or any other
//! grant, and `harw-macros` keeps no dependency on `harw-authority`: the
//! generated absolute paths resolve in the consumer crate, which must depend
//! on `harw-authority` itself.
//!
//! # Diagnostics
//! - an unknown permission name (the message lists the allowed names);
//! - a permission named twice;
//! - anything other than a comma-separated list of bare identifiers.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Ident, Token};

/// Accepted permission spellings and the matching `harw_authority::Permission`
/// variant, in `Permission::ALL` order.
///
/// Every variant of `harw_authority::Permission` must appear exactly once; the
/// unit test `permission_table_covers_every_authority_permission` checks this
/// against `Permission::ALL`, so a new variant cannot silently stay
/// unreachable from the macros.
pub(crate) const PERMISSION_VALUES: &[(&str, &str)] = &[
    ("read_workspace", "ReadWorkspace"),
    ("write_workspace", "WriteWorkspace"),
    ("execute_process", "ExecuteProcess"),
    ("network_access", "NetworkAccess"),
    ("read_secrets", "ReadSecrets"),
    ("manage_plugins", "ManagePlugins"),
    ("read_cargo_registry", "ReadCargoRegistry"),
    ("manage_containers", "ManageContainers"),
];

/// Looks a permission spelling up in [`PERMISSION_VALUES`] and returns the
/// variant identifier with the given span, or `None` for an unknown name.
pub(crate) fn permission_variant_ident(name: &str, span: Span) -> Option<Ident> {
    PERMISSION_VALUES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, variant)| Ident::new(variant, span))
}

/// Comma-separated list of the allowed spellings, each quoted with `quote`
/// (for example `"` or `` ` ``), for diagnostics.
pub(crate) fn allowed_permission_list(quote: char) -> String {
    PERMISSION_VALUES
        .iter()
        .map(|(key, _)| format!("{quote}{key}{quote}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Expands `permission_request!(name, ...)` into a
/// `::harw_authority::PermissionRequest`.
///
/// # Errors
/// - input that is not a comma-separated list of identifiers;
/// - an unknown permission name;
/// - a permission named more than once.
pub(crate) fn expand_permission_request(input: TokenStream) -> syn::Result<TokenStream> {
    let names = Punctuated::<Ident, Token![,]>::parse_terminated.parse2(input)?;

    let mut seen: Vec<String> = Vec::new();
    let mut variants: Vec<Ident> = Vec::new();
    for ident in names {
        let name = ident.to_string();
        let Some(variant) = permission_variant_ident(&name, ident.span()) else {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "unknown permission `{name}` in `permission_request!` (allowed: {})",
                    allowed_permission_list('`')
                ),
            ));
        };
        if seen.contains(&name) {
            return Err(syn::Error::new(
                ident.span(),
                format!("permission `{name}` is listed twice in `permission_request!`"),
            ));
        }
        seen.push(name);
        variants.push(variant);
    }

    if variants.is_empty() {
        return Ok(quote! { ::harw_authority::PermissionRequest::empty() });
    }
    Ok(quote! {
        ::harw_authority::PermissionRequest::from_permissions([
            #( ::harw_authority::Permission::#variants ),*
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Every `Permission` variant has exactly one spelling, and the spelling
    /// is the variant's serde name (`rename_all = "snake_case"`), so the macro
    /// vocabulary cannot drift from the wire vocabulary of `harw-authority`.
    #[test]
    fn permission_table_covers_every_authority_permission() -> TestResult {
        assert_eq!(
            PERMISSION_VALUES.len(),
            harw_tools::Permission::ALL.len(),
            "one table row per Permission variant"
        );
        for (permission, (key, variant)) in harw_tools::Permission::ALL
            .iter()
            .zip(PERMISSION_VALUES.iter())
        {
            let serde_name =
                serde_json::to_string(permission).map_err(ctx("Permission serializes"))?;
            assert_eq!(serde_name, format!("\"{key}\""), "{permission:?}");
            assert_eq!(format!("{permission:?}"), *variant);
        }
        Ok(())
    }

    /// `manage_containers` (the eighth variant, PL-97 §2.2 gap) resolves.
    #[test]
    fn manage_containers_resolves() -> TestResult {
        let ident = permission_variant_ident("manage_containers", Span::call_site()).ok_or(
            TestError::Missing("manage_containers must be a known permission"),
        )?;
        assert_eq!(ident.to_string(), "ManageContainers");
        Ok(())
    }

    #[test]
    fn expands_to_the_public_request_constructor_only() -> TestResult {
        let expanded = expand_permission_request(quote! { read_workspace, write_workspace })
            .map_err(ctx("a valid list expands"))?
            .to_string();
        assert_eq!(
            expanded,
            ":: harw_authority :: PermissionRequest :: from_permissions ([:: harw_authority :: \
             Permission :: ReadWorkspace , :: harw_authority :: Permission :: WriteWorkspace])"
        );
        assert!(!expanded.contains("PermissionSet"));
        assert!(!expanded.contains("from_policy"));
        Ok(())
    }

    #[test]
    fn empty_list_is_the_empty_request() -> TestResult {
        let expanded = expand_permission_request(TokenStream::new())
            .map_err(ctx("an empty list expands"))?
            .to_string();
        assert_eq!(
            expanded,
            ":: harw_authority :: PermissionRequest :: empty ()"
        );
        Ok(())
    }

    #[test]
    fn trailing_comma_is_accepted() -> TestResult {
        expand_permission_request(quote! { read_secrets, })
            .map_err(ctx("a trailing comma is accepted"))?;
        Ok(())
    }

    #[test]
    fn unknown_name_lists_the_allowed_names() -> TestResult {
        let Err(error) = expand_permission_request(quote! { read_workspace, root_access }) else {
            return Err(TestError::Unexpected(
                "an unknown permission must be a macro diagnostic".to_owned(),
            ));
        };
        let message = error.to_string();
        assert!(message.contains("`root_access`"), "{message}");
        assert!(message.contains("`manage_containers`"), "{message}");
        Ok(())
    }

    #[test]
    fn duplicate_name_is_rejected() -> TestResult {
        let Err(error) = expand_permission_request(quote! { read_workspace, read_workspace })
        else {
            return Err(TestError::Unexpected(
                "a duplicate permission must be a macro diagnostic".to_owned(),
            ));
        };
        assert!(error.to_string().contains("listed twice"));
        Ok(())
    }

    #[test]
    fn non_identifier_entries_are_rejected() -> TestResult {
        for tokens in [
            quote! { "read_workspace" },
            quote! { Permission::ReadWorkspace },
            quote! { read_workspace = true },
        ] {
            if expand_permission_request(tokens.clone()).is_ok() {
                return Err(TestError::Unexpected(format!(
                    "`{tokens}` must not be accepted as a permission list"
                )));
            }
        }
        Ok(())
    }
}
