//! `retention_classes!` expansion: one declaration list, everything derived.
//!
//! # Grammar
//!
//! ```ignore
//! harw_macros::retention_classes! {
//!     config = RetentionConfig;
//!
//!     /// Doc comment, forwarded to the config field.
//!     tui_log: ephemeral {
//!         dir = dirs::tui_log,                    // fn(&Roots) -> Vec<PathBuf>
//!         name = prefix_suffix("tui", ".log"),    // any | prefix(..) | suffix(..) | contains(..) | prefix_suffix(..)
//!         max_age_secs = 1_209_600,               // integer > 0, or `none`
//!         max_bytes = 52_428_800,
//!         max_files = none,
//!         keep_newest = 1,                        // optional, default 1
//!     }
//!     // `ephemeral` | `security_relevant`
//! }
//! ```
//!
//! `max_age_secs`, `max_bytes` and `max_files` are mandatory (so every class
//! states its limits, even `none`). Each key appears at most once.
//!
//! # Generated items (in the calling module)
//!
//! - `pub struct <config>`: one `::harw_retention::ClassConfig` field per
//!   class id, `#[serde(default, deny_unknown_fields)]`, plus
//!   `validate()`, `class_config(id)` and `class_config_mut(id)`;
//! - `pub static CLASSES: &[::harw_retention::Class]` (declaration order);
//! - `pub fn policy_for(&<config>, id) -> Option<ResolvedClass>`, which
//!   applies the opt-in rules (security-relevant classes default to off) and
//!   `pub fn resolve_all(&<config>) -> Vec<ResolvedClass>`.
//!
//! All paths are textual (`::harw_retention::..`); the crate invoking the
//! macro is `harw-retention` itself (via `extern crate self`) or any crate
//! depending on it. `harw-macros` does not depend on `harw-retention`.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Ident, LitInt, LitStr, Path, Token};

enum Matcher {
    Any,
    Prefix(LitStr),
    Suffix(LitStr),
    Contains(LitStr),
    PrefixSuffix(LitStr, LitStr),
}

struct Limit(Option<LitInt>);

struct ClassDecl {
    attrs: Vec<Attribute>,
    id: Ident,
    security: bool,
    dir: Path,
    name: Matcher,
    max_age_secs: Limit,
    max_bytes: Limit,
    max_files: Limit,
    keep_newest: Option<LitInt>,
}

struct Input {
    config: Ident,
    classes: Vec<ClassDecl>,
}

fn parse_limit(input: ParseStream<'_>) -> syn::Result<Limit> {
    if input.peek(Ident) {
        let ident: Ident = input.parse()?;
        if ident == "none" {
            return Ok(Limit(None));
        }
        return Err(syn::Error::new_spanned(
            ident,
            "expected an integer greater than 0 or `none`",
        ));
    }
    let lit: LitInt = input.parse()?;
    if lit.base10_parse::<u64>()? == 0 {
        return Err(syn::Error::new_spanned(
            lit,
            "a limit of 0 would delete everything; use `none` for no limit",
        ));
    }
    Ok(Limit(Some(lit)))
}

fn parse_matcher(input: ParseStream<'_>) -> syn::Result<Matcher> {
    let ident: Ident = input.parse()?;
    match ident.to_string().as_str() {
        "any" => Ok(Matcher::Any),
        "prefix" | "suffix" | "contains" => {
            let content;
            syn::parenthesized!(content in input);
            let lit: LitStr = content.parse()?;
            Ok(match ident.to_string().as_str() {
                "prefix" => Matcher::Prefix(lit),
                "suffix" => Matcher::Suffix(lit),
                _ => Matcher::Contains(lit),
            })
        }
        "prefix_suffix" => {
            let content;
            syn::parenthesized!(content in input);
            let prefix: LitStr = content.parse()?;
            content.parse::<Token![,]>()?;
            let suffix: LitStr = content.parse()?;
            Ok(Matcher::PrefixSuffix(prefix, suffix))
        }
        other => Err(syn::Error::new_spanned(
            ident,
            format!(
                "unknown name matcher `{other}` (expected `any`, `prefix(..)`, `suffix(..)`, `contains(..)` \
                 or `prefix_suffix(.., ..)`)"
            ),
        )),
    }
}

impl Parse for ClassDecl {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let id: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let kind: Ident = input.parse()?;
        let security = match kind.to_string().as_str() {
            "ephemeral" => false,
            "security_relevant" => true,
            other => {
                return Err(syn::Error::new_spanned(
                    kind,
                    format!(
                        "unknown class kind `{other}` (expected `ephemeral` or `security_relevant`)"
                    ),
                ));
            }
        };
        let body;
        syn::braced!(body in input);

        let mut dir = None;
        let mut name = None;
        let mut max_age_secs = None;
        let mut max_bytes = None;
        let mut max_files = None;
        let mut keep_newest = None;
        let mut seen = HashSet::new();
        while !body.is_empty() {
            let key: Ident = body.parse()?;
            if !seen.insert(key.to_string()) {
                return Err(syn::Error::new_spanned(
                    &key,
                    format!("key `{key}` given twice in class `{id}`"),
                ));
            }
            body.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "dir" => dir = Some(body.parse::<Path>()?),
                "name" => name = Some(parse_matcher(&body)?),
                "max_age_secs" => max_age_secs = Some(parse_limit(&body)?),
                "max_bytes" => max_bytes = Some(parse_limit(&body)?),
                "max_files" => max_files = Some(parse_limit(&body)?),
                "keep_newest" => keep_newest = Some(body.parse::<LitInt>()?),
                other => {
                    return Err(syn::Error::new_spanned(
                        key,
                        format!(
                            "unknown key `{other}` (expected dir, name, max_age_secs, \
                             max_bytes, max_files, keep_newest)"
                        ),
                    ));
                }
            }
            if !body.is_empty() {
                body.parse::<Token![,]>()?;
            }
        }
        let missing = |what: &str| {
            syn::Error::new_spanned(&id, format!("class `{id}` is missing `{what} = ..`"))
        };
        Ok(Self {
            dir: dir.ok_or_else(|| missing("dir"))?,
            name: name.ok_or_else(|| missing("name"))?,
            max_age_secs: max_age_secs.ok_or_else(|| missing("max_age_secs"))?,
            max_bytes: max_bytes.ok_or_else(|| missing("max_bytes"))?,
            max_files: max_files.ok_or_else(|| missing("max_files"))?,
            keep_newest,
            attrs,
            id,
            security,
        })
    }
}

impl Parse for Input {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "config" {
            return Err(syn::Error::new_spanned(
                key,
                "the declaration must start with `config = <StructName>;`",
            ));
        }
        input.parse::<Token![=]>()?;
        let config: Ident = input.parse()?;
        input.parse::<Token![;]>()?;
        let mut classes = Vec::new();
        while !input.is_empty() {
            classes.push(input.parse::<ClassDecl>()?);
        }
        Ok(Self { config, classes })
    }
}

fn limit_tokens(limit: &Limit) -> TokenStream {
    match &limit.0 {
        Some(lit) => quote! { ::core::option::Option::Some(#lit) },
        None => quote! { ::core::option::Option::None },
    }
}

fn matcher_tokens(matcher: &Matcher) -> TokenStream {
    match matcher {
        Matcher::Any => quote! { ::harw_retention::NameMatch::any() },
        Matcher::Prefix(p) => quote! { ::harw_retention::NameMatch::prefix(#p) },
        Matcher::Suffix(s) => quote! { ::harw_retention::NameMatch::suffix(#s) },
        Matcher::Contains(c) => quote! { ::harw_retention::NameMatch::contains(#c) },
        Matcher::PrefixSuffix(p, s) => {
            quote! { ::harw_retention::NameMatch::prefix_suffix(#p, #s) }
        }
    }
}

/// Expands a `retention_classes!` declaration.
///
/// # Errors
/// A `syn::Error` for syntax errors, duplicate class ids, unknown kinds,
/// matchers or keys, missing mandatory keys and zero limits.
pub(crate) fn expand_retention_classes(input: TokenStream) -> syn::Result<TokenStream> {
    let parsed: Input = syn::parse2(input)?;
    if parsed.classes.is_empty() {
        return Err(syn::Error::new_spanned(
            &parsed.config,
            "declare at least one retention class",
        ));
    }
    let mut seen = HashSet::new();
    for class in &parsed.classes {
        if !seen.insert(class.id.to_string()) {
            return Err(syn::Error::new_spanned(
                &class.id,
                format!("class `{}` is declared twice", class.id),
            ));
        }
    }

    let config = &parsed.config;
    let ids: Vec<&Ident> = parsed.classes.iter().map(|c| &c.id).collect();
    let id_strs: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    let attrs: Vec<&Vec<Attribute>> = parsed.classes.iter().map(|c| &c.attrs).collect();

    let rows = parsed.classes.iter().map(|c| {
        let id = c.id.to_string();
        let kind = if c.security {
            quote! { ::harw_retention::ClassKind::SecurityRelevant }
        } else {
            quote! { ::harw_retention::ClassKind::Ephemeral }
        };
        let name = matcher_tokens(&c.name);
        let age = limit_tokens(&c.max_age_secs);
        let bytes = limit_tokens(&c.max_bytes);
        let files = limit_tokens(&c.max_files);
        let keep = c
            .keep_newest
            .as_ref()
            .map_or_else(|| quote! { 1 }, |lit| quote! { #lit });
        let dir = &c.dir;
        quote! {
            ::harw_retention::Class {
                id: #id,
                kind: #kind,
                name_match: #name,
                defaults: ::harw_retention::ClassDefaults {
                    max_age_secs: #age,
                    max_bytes: #bytes,
                    max_files: #files,
                    keep_newest: #keep,
                },
                resolve_dirs: #dir,
            }
        }
    });

    Ok(quote! {
        /// `[retention]` config: one override table per declared class.
        ///
        /// Generated by `retention_classes!`; an unset key means "use the
        /// class default" (see `CLASSES`).
        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::default::Default,
            ::core::cmp::PartialEq,
            ::core::cmp::Eq,
            ::harw_retention::serde::Serialize,
            ::harw_retention::serde::Deserialize,
        )]
        #[serde(crate = "::harw_retention::serde", deny_unknown_fields)]
        pub struct #config {
            #(
                #(#attrs)*
                #[serde(default, skip_serializing_if = "::harw_retention::ClassConfig::is_unset")]
                pub #ids: ::harw_retention::ClassConfig,
            )*
        }

        /// Registry of all declared classes, in declaration order.
        pub static CLASSES: &[::harw_retention::Class] = &[ #(#rows),* ];

        impl #config {
            /// The override table of class `id`, if declared.
            #[must_use]
            pub fn class_config(&self, id: &str) -> ::core::option::Option<&::harw_retention::ClassConfig> {
                match id {
                    #( #id_strs => ::core::option::Option::Some(&self.#ids), )*
                    _ => ::core::option::Option::None,
                }
            }

            /// Mutable access to the override table of class `id`.
            #[must_use]
            pub fn class_config_mut(&mut self, id: &str) -> ::core::option::Option<&mut ::harw_retention::ClassConfig> {
                match id {
                    #( #id_strs => ::core::option::Option::Some(&mut self.#ids), )*
                    _ => ::core::option::Option::None,
                }
            }

            /// Checks every class table (`0` limits are rejected).
            ///
            /// # Errors
            /// A message naming `retention.<class>.<key>`.
            pub fn validate(&self) -> ::core::result::Result<(), ::std::string::String> {
                #(
                    self.#ids
                        .validate()
                        .map_err(|e| ::std::format!("retention.{}.{e}", #id_strs))?;
                )*
                ::core::result::Result::Ok(())
            }
        }

        /// The effective policy inputs of class `id` under `cfg`, with the
        /// opt-in rules applied (security-relevant classes are off unless
        /// config says `enabled = true`). `None` for an unknown id.
        #[must_use]
        pub fn policy_for(cfg: &#config, id: &str) -> ::core::option::Option<::harw_retention::ResolvedClass> {
            let class = CLASSES.iter().find(|c| c.id == id)?;
            let class_cfg = cfg.class_config(id)?;
            ::core::option::Option::Some(::harw_retention::ResolvedClass::resolve(class, class_cfg))
        }

        /// [`policy_for`] for every declared class, in declaration order.
        #[must_use]
        pub fn resolve_all(cfg: &#config) -> ::std::vec::Vec<::harw_retention::ResolvedClass> {
            CLASSES
                .iter()
                .filter_map(|c| policy_for(cfg, c.id))
                .collect()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn err_of(input: TokenStream) -> TestResult<String> {
        match expand_retention_classes(input) {
            Err(e) => Ok(e.to_string()),
            Ok(_) => Err(TestError::Unexpected("expected a compile error".to_owned())),
        }
    }

    #[test]
    fn test_rejects_duplicate_class_ids() -> TestResult {
        let msg = err_of(quote! {
            config = C;
            a: ephemeral { dir = d, name = any, max_age_secs = 1, max_bytes = none, max_files = none }
            a: ephemeral { dir = d, name = any, max_age_secs = 1, max_bytes = none, max_files = none }
        })?;
        assert!(msg.contains("declared twice"), "{msg}");
        Ok(())
    }

    #[test]
    fn test_rejects_zero_limit_unknown_kind_and_missing_key() -> TestResult {
        let zero = err_of(quote! {
            config = C;
            a: ephemeral { dir = d, name = any, max_age_secs = 0, max_bytes = none, max_files = none }
        })?;
        assert!(zero.contains("0 would delete everything"), "{zero}");
        let kind = err_of(quote! {
            config = C;
            a: secret { dir = d, name = any, max_age_secs = 1, max_bytes = none, max_files = none }
        })?;
        assert!(kind.contains("unknown class kind"), "{kind}");
        let missing = err_of(quote! {
            config = C;
            a: ephemeral { dir = d, name = any, max_age_secs = 1, max_bytes = none }
        })?;
        assert!(missing.contains("missing `max_files"), "{missing}");
        Ok(())
    }

    #[test]
    fn test_rejects_unknown_matcher_and_duplicate_key() -> TestResult {
        let matcher = err_of(quote! {
            config = C;
            a: ephemeral { dir = d, name = glob("x"), max_age_secs = 1, max_bytes = none, max_files = none }
        })?;
        assert!(matcher.contains("unknown name matcher"), "{matcher}");
        let dup = err_of(quote! {
            config = C;
            a: ephemeral { dir = d, dir = e, name = any, max_age_secs = 1, max_bytes = none, max_files = none }
        })?;
        assert!(dup.contains("given twice"), "{dup}");
        Ok(())
    }

    #[test]
    fn test_valid_declaration_expands() -> TestResult {
        let out = expand_retention_classes(quote! {
            config = C;
            a: security_relevant { dir = d, name = prefix_suffix("p", ".s"), max_age_secs = 1,
                max_bytes = none, max_files = 3, keep_newest = 2 }
        })
        .map_err(|e| TestError::Unexpected(e.to_string()))?
        .to_string();
        assert!(out.contains("SecurityRelevant"));
        assert!(out.contains("pub static CLASSES"));
        assert!(out.contains("pub fn policy_for"));
        Ok(())
    }
}
