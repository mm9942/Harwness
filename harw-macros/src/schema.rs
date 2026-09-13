//! JSON-Schema-Inferenz für `#[derive(Tool)]`-Argumentstrukturen.
//!
//! Bildet Rust-Feldtypen (`String`, Zahlen, `bool`, `Vec<T>`, `Option<T>`) auf
//! `::harw_tools::JsonSchema`-Ausdrücke ab und liest `///`-Doc-Kommentare
//! sowie `#[tool(default = ...)]`-Attribute als Schema-Metadaten ein. Alle
//! Funktionen sind `pub(crate)`, damit spätere Makros (z. B. ein künftiges
//! `OpArgs`-Derive) dieselbe Schema-Inferenz wiederverwenden können, ohne den
//! Code zu duplizieren.

use quote::quote;
use syn::Type;

/// Map a Rust type onto a JSON-Schema expression. Returns the schema token
/// stream plus whether the field is optional (`Option<T>`).
pub(crate) fn schema_for_type(
    ty: &Type,
    description: Option<&str>,
    default: Option<&syn::Expr>,
) -> syn::Result<(proc_macro2::TokenStream, bool)> {
    if let Some(inner) = option_inner(ty) {
        let (schema, _) = schema_for_type(inner, description, default)?;
        return Ok((schema, true));
    }

    let desc_tokens = match description {
        Some(d) => quote! { description: ::core::option::Option::Some(#d.to_string()), },
        None => quote! {},
    };

    if let Some(inner) = vec_inner(ty) {
        let (item_schema, _) = schema_for_type(inner, None, None)?;
        let default_tokens = json_schema_default(default);
        let schema = quote! {
            ::harw_tools::JsonSchema {
                schema_type: ::core::option::Option::Some(::harw_tools::JsonSchemaType::Array),
                #desc_tokens
                items: ::core::option::Option::Some(::std::boxed::Box::new(#item_schema)),
                #default_tokens
                ..::core::default::Default::default()
            }
        };
        return Ok((schema, false));
    }

    let json_type = scalar_json_type(ty).ok_or_else(|| {
        syn::Error::new_spanned(
            ty,
            "unsupported tool field type; expected a supported JSON type",
        )
    })?;
    let default_tokens = json_schema_default(default);
    let schema = quote! {
        ::harw_tools::JsonSchema {
            schema_type: ::core::option::Option::Some(#json_type),
            #desc_tokens
            #default_tokens
            ..::core::default::Default::default()
        }
    };
    Ok((schema, false))
}

/// Emit the `JsonSchema::default` assignment for a previously validated
/// JSON-literal expression. Keeping this separate ensures a field receives no
/// invented schema default when its attribute has no safely serializable value.
pub(crate) fn json_schema_default(default: Option<&syn::Expr>) -> proc_macro2::TokenStream {
    default
        .map(|expr| {
            quote! {
                default: ::core::option::Option::Some(::harw_tools::serde_json::json!(#expr)),
            }
        })
        .unwrap_or_default()
}

/// Map a scalar Rust type to its `JsonSchemaType` token stream.
pub(crate) fn scalar_json_type(ty: &Type) -> Option<proc_macro2::TokenStream> {
    let ident = type_last_ident(ty);
    let name = ident.as_deref().unwrap_or("");
    match name {
        "String" | "str" => Some(quote! { ::harw_tools::JsonSchemaType::String }),
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128"
        | "isize" => Some(quote! { ::harw_tools::JsonSchemaType::Integer }),
        "f32" | "f64" => Some(quote! { ::harw_tools::JsonSchemaType::Number }),
        "bool" => Some(quote! { ::harw_tools::JsonSchemaType::Boolean }),
        _ => None,
    }
}

/// The final path-segment identifier of a type, e.g. `String` from
/// `std::string::String`.
pub(crate) fn type_last_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
        Type::Reference(r) => type_last_ident(&r.elem),
        _ => None,
    }
}

/// If `ty` is `Option<T>`, return `T`.
pub(crate) fn option_inner(ty: &Type) -> Option<&Type> {
    generic_inner(ty, "Option")
}

/// If `ty` is `Vec<T>`, return `T`.
pub(crate) fn vec_inner(ty: &Type) -> Option<&Type> {
    generic_inner(ty, "Vec")
}

pub(crate) fn generic_inner<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    if let Type::Path(tp) = ty {
        let seg = tp.path.segments.last()?;
        if seg.ident == wrapper {
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                for arg in &args.args {
                    if let syn::GenericArgument::Type(inner) = arg {
                        return Some(inner);
                    }
                }
            }
        }
    }
    None
}

/// Join `///` doc comments into a single trimmed description string.
pub(crate) fn doc_string(attrs: &[syn::Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(expr_lit) = &nv.value {
                if let syn::Lit::Str(s) = &expr_lit.lit {
                    lines.push(s.value().trim().to_string());
                }
            }
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join(" ").trim().to_string())
    }
}

/// Parse a field-level `#[tool(default = ...)]` marker.
pub(crate) fn field_default(field: &syn::Field) -> syn::Result<Option<syn::Expr>> {
    let mut default = None;
    for attr in &field.attrs {
        if !attr.path().is_ident("tool") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default") {
                if default.is_some() {
                    return Err(meta.error("duplicate `tool(default = ...)` attribute"));
                }
                if meta.input.peek(syn::Token![=]) {
                    let expr: syn::Expr = meta.value()?.parse()?;
                    if !is_json_literal_expr(&expr) {
                        return Err(syn::Error::new_spanned(
                            expr,
                            "tool default must be a JSON-compatible literal expression",
                        ));
                    }
                    default = Some(expr);
                }
                Ok(())
            } else {
                Err(meta.error("unsupported `tool` field attribute key (expected `default`)"))
            }
        })?;
    }
    Ok(default)
}

pub(crate) fn is_json_literal_expr(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Lit(expr) => matches!(
            expr.lit,
            syn::Lit::Bool(_)
                | syn::Lit::Byte(_)
                | syn::Lit::ByteStr(_)
                | syn::Lit::Char(_)
                | syn::Lit::Float(_)
                | syn::Lit::Int(_)
                | syn::Lit::Str(_)
        ),
        syn::Expr::Unary(expr) if matches!(expr.op, syn::UnOp::Neg(_)) => {
            matches!(expr.expr.as_ref(), syn::Expr::Lit(lit) if matches!(
                lit.lit,
                syn::Lit::Float(_) | syn::Lit::Int(_)
            ))
        }
        syn::Expr::Array(expr) => expr.elems.iter().all(is_json_literal_expr),
        _ => false,
    }
}
