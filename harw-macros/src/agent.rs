//! `#[agent]`-Expansion.
//!
//! Validiert die deklarativen `#[agent(name = ..., role = ..., description =
//! ...)]`-Argumente und reicht die annotierte Funktion unverändert durch, da
//! die aktuelle Laufzeit noch keine Agent-Registrierung/-Ausführung kennt.
//! `parse_agent_args` und `expand_agent` sind die Einstiegspunkte, die die
//! `#[proc_macro_attribute] agent`-Funktion im Crate-Root (`lib.rs`) aufruft.

use quote::quote;
use syn::LitStr;
use syn::parse::Parser;

#[derive(Debug, Default)]
pub(crate) struct AgentArgs {
    name: Option<LitStr>,
    role: Option<LitStr>,
    description: Option<LitStr>,
}

pub(crate) fn parse_agent_args(attr: proc_macro2::TokenStream) -> syn::Result<AgentArgs> {
    let mut args = AgentArgs::default();
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") {
            if args.name.is_some() {
                return Err(meta.error("duplicate `agent` attribute field `name`"));
            }
            args.name = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("role") {
            if args.role.is_some() {
                return Err(meta.error("duplicate `agent` attribute field `role`"));
            }
            args.role = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("description") {
            if args.description.is_some() {
                return Err(meta.error("duplicate `agent` attribute field `description`"));
            }
            args.description = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error(
                "unknown `agent` attribute field; expected `name`, `role`, or `description`",
            ))
        }
    });
    parser.parse2(attr)?;
    Ok(args)
}

pub(crate) fn expand_agent(
    item: syn::Item,
    _args: AgentArgs,
) -> syn::Result<proc_macro2::TokenStream> {
    match item {
        syn::Item::Fn(function) => Ok(quote! { #function }),
        other => Err(syn::Error::new_spanned(
            other,
            "#[agent] may only be applied to a function",
        )),
    }
}

#[cfg(test)]
mod agent_tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn accepts_supported_fields_once() -> TestResult {
        let args = parse_agent_args(quote! {
            name = "worker", role = "coding", description = "bounded task"
        })
        .map_err(ctx("supported agent fields should parse"))?;

        assert_eq!(
            args.name.as_ref().map(LitStr::value),
            Some("worker".to_owned())
        );
        assert_eq!(
            args.role.as_ref().map(LitStr::value),
            Some("coding".to_owned())
        );
        assert_eq!(
            args.description.as_ref().map(LitStr::value),
            Some("bounded task".to_owned())
        );
        Ok(())
    }

    #[test]
    fn rejects_unknown_field() -> TestResult {
        let Err(error) = parse_agent_args(quote!(model = "gpt-5")) else {
            return Err(TestError::Unexpected(
                "unknown agent fields must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("unknown `agent` attribute field")
        );
        Ok(())
    }

    #[test]
    fn rejects_duplicate_field() -> TestResult {
        let Err(error) = parse_agent_args(quote!(name = "one", name = "two")) else {
            return Err(TestError::Unexpected(
                "duplicate agent fields must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("duplicate `agent` attribute field `name`")
        );
        Ok(())
    }

    #[test]
    fn rejects_non_function_items() -> TestResult {
        let item: syn::Item = syn::parse_quote!(
            struct Worker;
        );
        let Err(error) = expand_agent(item, AgentArgs::default()) else {
            return Err(TestError::Unexpected(
                "agent must only annotate functions".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "#[agent] may only be applied to a function"
        );
        Ok(())
    }
}
