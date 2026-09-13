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

    #[test]
    fn accepts_supported_fields_once() {
        let args = parse_agent_args(quote! {
            name = "worker", role = "coding", description = "bounded task"
        })
        .expect("supported agent fields should parse");

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
    }

    #[test]
    fn rejects_unknown_field() {
        let error = parse_agent_args(quote!(model = "gpt-5"))
            .expect_err("unknown agent fields must be rejected");
        assert!(
            error
                .to_string()
                .contains("unknown `agent` attribute field")
        );
    }

    #[test]
    fn rejects_duplicate_field() {
        let error = parse_agent_args(quote!(name = "one", name = "two"))
            .expect_err("duplicate agent fields must be rejected");
        assert!(
            error
                .to_string()
                .contains("duplicate `agent` attribute field `name`")
        );
    }

    #[test]
    fn rejects_non_function_items() {
        let item: syn::Item = syn::parse_quote!(
            struct Worker;
        );
        let error = expand_agent(item, AgentArgs::default())
            .expect_err("agent must only annotate functions");
        assert_eq!(
            error.to_string(),
            "#[agent] may only be applied to a function"
        );
    }
}
