//! `#[derive(HarwError)]`-Expansion.
//!
//! Enthält die vollständige Code-Erzeugung für die `HarwError`-Ableitung:
//! Sammeln der Varianten-Metadaten (`VariantInfo`), Aufbau der `Display`- und
//! `Error::source`-Match-Arme, `From`-Impls für `#[from]`-Varianten sowie den
//! optionalen `<Prefix>Result<T>`-Typalias. `expand_harw_error` ist der
//! Einstiegspunkt, den die `#[proc_macro_derive(HarwError, ...)]`-Funktion im
//! Crate-Root (`lib.rs`) aufruft.

use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident, LitStr};

struct VariantInfo {
    ident: Ident,
    fields: Fields,
    msg: Option<LitStr>,
    is_from: bool,
    /// Field-level `#[source]` / `#[from]` marker: index of the field that
    /// `source()` returns. Set for tuple and named variants.
    source_field: Option<usize>,
    /// The `#[from]` sits on the single field of a named (struct) variant.
    from_named: bool,
}

pub(crate) fn expand_harw_error(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let enum_name = &input.ident;

    let data = match &input.data {
        Data::Enum(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "HarwError can only be derived for enums",
            ));
        }
    };

    let mut variants = Vec::new();
    for variant in &data.variants {
        let mut msg = None;
        let mut is_from = false;

        for attr in &variant.attrs {
            if attr.path().is_ident("msg") {
                let lit: LitStr = attr.parse_args()?;
                msg = Some(lit);
            } else if attr.path().is_ident("from") {
                is_from = true;
            }
        }

        let mut source_field = None;
        let mut from_named = false;
        for (idx, field) in variant.fields.iter().enumerate() {
            let mut has_source = false;
            let mut has_from = false;
            for attr in &field.attrs {
                if attr.path().is_ident("source") {
                    has_source = true;
                } else if attr.path().is_ident("from") {
                    has_from = true;
                }
            }
            if !has_source && !has_from {
                continue;
            }
            if source_field.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "only one field per variant may carry #[source] or #[from]",
                ));
            }
            if has_from {
                if is_from {
                    return Err(syn::Error::new_spanned(
                        field,
                        "#[from] on both the variant and a field is redundant",
                    ));
                }
                if !matches!(&variant.fields, Fields::Named(f) if f.named.len() == 1) {
                    return Err(syn::Error::new_spanned(
                        field,
                        "field-level #[from] requires a named variant with exactly one field",
                    ));
                }
                from_named = true;
            }
            source_field = Some(idx);
        }
        if is_from && source_field.is_some() {
            return Err(syn::Error::new_spanned(
                &variant.ident,
                "#[from] already wires source(); drop the field-level #[source]",
            ));
        }

        if is_from {
            let ok = matches!(&variant.fields, Fields::Unnamed(f) if f.unnamed.len() == 1);
            if !ok {
                return Err(syn::Error::new_spanned(
                    &variant.ident,
                    "#[from] requires a single-field tuple variant",
                ));
            }
        }

        variants.push(VariantInfo {
            ident: variant.ident.clone(),
            fields: variant.fields.clone(),
            msg,
            is_from,
            source_field,
            from_named,
        });
    }

    let display_arms = variants
        .iter()
        .map(display_arm)
        .collect::<syn::Result<Vec<_>>>()?;
    let source_arms: Vec<_> = variants.iter().map(source_arm).collect();
    let from_impls: Vec<_> = variants
        .iter()
        .filter_map(|v| from_impl(enum_name, v))
        .collect();

    let result_alias = result_alias(enum_name);

    Ok(quote! {
        impl ::core::fmt::Display for #enum_name {
            // Die Match-Arme binden jedes Feld einer Variante, damit sie im
            // `#[msg]` interpoliert werden *können*. Ein Feld, das dort nicht
            // vorkommt, ist kein Versehen, sondern der Regelfall einer
            // bewussten Entwurfsentscheidung: mehrere Teilbäume dieses
            // Workspace fordern **inhaltsfreie** Fehlermeldungen, weil eine
            // Meldung geloggt wird und was geloggt wird, den Host verlässt.
            // Der Detailwert bleibt über `source()` erreichbar — für den, der
            // ihn bewusst abholt.
            //
            // Ohne dieses `allow` wäre eine inhaltsfreie Meldung nicht
            // ausdrückbar, ohne unter `-D warnings` zu brechen.
            #[allow(unused_variables)]
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self {
                    #(#display_arms)*
                }
            }
        }

        impl ::std::error::Error for #enum_name {
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                match self {
                    #(#source_arms)*
                }
            }
        }

        #(#from_impls)*

        #result_alias
    })
}

/// Build one `Display` match arm for a variant.
fn display_arm(v: &VariantInfo) -> syn::Result<proc_macro2::TokenStream> {
    let ident = &v.ident;

    match &v.fields {
        Fields::Unit => {
            if let Some(msg) = &v.msg {
                let fmt = rewrite_format(&msg.value());
                let fmt = LitStr::new(&fmt, msg.span());
                Ok(quote! { Self::#ident => ::core::write!(f, #fmt), })
            } else {
                let name = ident.to_string();
                Ok(quote! { Self::#ident => f.write_str(#name), })
            }
        }
        Fields::Unnamed(fields) => {
            let bindings: Vec<Ident> = (0..fields.unnamed.len())
                .map(|i| Ident::new(&format!("f{i}"), ident.span()))
                .collect();
            if let Some(msg) = &v.msg {
                let fmt = rewrite_format(&msg.value());
                let fmt = LitStr::new(&fmt, msg.span());
                Ok(quote! {
                    Self::#ident( #(#bindings),* ) => ::core::write!(f, #fmt),
                })
            } else if v.is_from {
                // No explicit message: defer to the inner error's Display.
                let inner = &bindings[0];
                Ok(quote! {
                    Self::#ident( #(#bindings),* ) => ::core::fmt::Display::fmt(#inner, f),
                })
            } else {
                let name = ident.to_string();
                Ok(quote! {
                    Self::#ident( #(#bindings),* ) => f.write_str(#name),
                })
            }
        }
        Fields::Named(fields) => {
            let names: Vec<Ident> = fields
                .named
                .iter()
                .map(|f| {
                    f.ident.clone().ok_or_else(|| {
                        syn::Error::new_spanned(f, "HarwError requires every field to be named")
                    })
                })
                .collect::<syn::Result<Vec<Ident>>>()?;
            if let Some(msg) = &v.msg {
                let fmt = rewrite_format(&msg.value());
                let fmt = LitStr::new(&fmt, msg.span());
                Ok(quote! {
                    Self::#ident { #(#names),* } => ::core::write!(f, #fmt),
                })
            } else if v.from_named {
                let inner = &names[0];
                Ok(quote! {
                    Self::#ident { #inner } => ::core::fmt::Display::fmt(#inner, f),
                })
            } else {
                let name = ident.to_string();
                Ok(quote! {
                    Self::#ident { .. } => f.write_str(#name),
                })
            }
        }
    }
}

/// Build one `Error::source` match arm for a variant.
fn source_arm(v: &VariantInfo) -> proc_macro2::TokenStream {
    let ident = &v.ident;
    if v.is_from {
        quote! {
            Self::#ident(inner) => ::core::option::Option::Some(inner),
        }
    } else if let Some(idx) = v.source_field {
        match &v.fields {
            Fields::Named(fields) => {
                let name = fields.named.iter().nth(idx).and_then(|f| f.ident.clone());
                quote! { Self::#ident { #name, .. } => ::core::option::Option::Some(#name), }
            }
            Fields::Unnamed(fields) => {
                let pats = (0..fields.unnamed.len()).map(|i| {
                    if i == idx {
                        quote! { inner }
                    } else {
                        quote! { _ }
                    }
                });
                quote! { Self::#ident( #(#pats),* ) => ::core::option::Option::Some(inner), }
            }
            Fields::Unit => quote! { Self::#ident => ::core::option::Option::None, },
        }
    } else {
        match &v.fields {
            Fields::Unit => quote! { Self::#ident => ::core::option::Option::None, },
            Fields::Unnamed(_) => {
                quote! { Self::#ident(..) => ::core::option::Option::None, }
            }
            Fields::Named(_) => {
                quote! { Self::#ident { .. } => ::core::option::Option::None, }
            }
        }
    }
}

/// Build a `From<Inner>` impl for a `#[from]` variant.
fn from_impl(enum_name: &Ident, v: &VariantInfo) -> Option<proc_macro2::TokenStream> {
    if !v.is_from && !v.from_named {
        return None;
    }
    let ident = &v.ident;
    let (ty, ctor) = match &v.fields {
        Fields::Unnamed(fields) => (&fields.unnamed.first()?.ty, None),
        Fields::Named(fields) => {
            let f = fields.named.first()?;
            (&f.ty, f.ident.clone())
        }
        Fields::Unit => return None,
    };
    let build = match ctor {
        Some(name) => quote! { #enum_name::#ident { #name: value } },
        None => quote! { #enum_name::#ident(value) },
    };
    Some(quote! {
        impl ::core::convert::From<#ty> for #enum_name {
            fn from(value: #ty) -> Self {
                #build
            }
        }
    })
}

/// Emit `pub type <Prefix>Result<T> = Result<T, <Enum>>;` when the enum name
/// ends in `Error`.
fn result_alias(enum_name: &Ident) -> proc_macro2::TokenStream {
    let name = enum_name.to_string();
    if let Some(prefix) = name.strip_suffix("Error") {
        if !prefix.is_empty() {
            let alias = Ident::new(&format!("{prefix}Result"), enum_name.span());
            return quote! {
                pub type #alias<T> = ::core::result::Result<T, #enum_name>;
            };
        }
    }
    proc_macro2::TokenStream::new()
}

/// Rewrite `#[msg("...")]` format strings so positional `{0}` style references
/// map onto the synthetic `f0`, `f1`, ... tuple bindings, while named `{field}`
/// references and `{{`/`}}` escapes are preserved verbatim.
fn rewrite_format(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push_str("{{");
                    continue;
                }
                // Collect the argument-name portion (up to ':' or '}').
                let mut name = String::new();
                while let Some(&next) = chars.peek() {
                    if next == ':' || next == '}' {
                        break;
                    }
                    name.push(next);
                    chars.next();
                }
                out.push('{');
                if !name.is_empty() && name.chars().all(|ch| ch.is_ascii_digit()) {
                    out.push('f');
                }
                out.push_str(&name);
                // Remaining format spec (':...' and closing '}') is copied as-is
                // by the outer loop.
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push_str("}}");
                } else {
                    out.push('}');
                }
            }
            other => out.push(other),
        }
    }

    out
}
