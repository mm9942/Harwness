//! `#[derive(Tool)]` und `#[tool]`-Expansion.
//!
//! `expand_tool` erzeugt aus einer Argumentstruktur einen `tool_spec()`-
//! Konstruktor (JSON-Schema via [`crate::schema`]); `parse_tool_attr` liest den
//! rohen `#[tool(...)]`-Attribut-Token-Stream in [`ToolAttr`] ein und
//! `expand_tool_fn` verwandelt eine `async fn` in eine
//! `ToolExecutor`-Implementierung. Diese drei Funktionen sind die
//! Einstiegspunkte, die das Crate-Root (`lib.rs`) aus den
//! `#[proc_macro_derive(Tool, ...)]`- bzw. `#[proc_macro_attribute] tool`-
//! Funktionen aufruft.
//!
//! # Sicherheits-Prologe
//!
//! `expand_tool_fn` erzeugt aus den optionalen Attribut-Schlüsseln
//! `permission` und `host_from` Sandbox-Prüfungen am Anfang von
//! `ToolExecutor::execute`. Zwei Invarianten sind dabei bewusst gewählt und
//! dürfen nicht aufgeweicht werden:
//!
//! 1. **Permission vor Deserialisierung.** Der `require_permission`-Prolog
//!    steht vor `serde_json::from_value`, damit vom Modell kontrollierte
//!    Argumente keinerlei Arbeit auslösen, bevor die Autorität geprüft ist.
//! 2. **Fail-closed bei unparsebarem Host.** Liefert `host_from_url` `None`,
//!    bricht der generierte Code mit einem `ToolOutput::Error` ab, statt einen
//!    leeren Hostnamen in die Scope-Prüfung zu geben. Ein leerer Host würde
//!    andernfalls an `NetworkScope::allows("")` weitergereicht — ein
//!    Fail-open-Risiko, falls diese Prüfung leere Eingaben permissiv behandelt.

use crate::schema::{doc_string, field_default, schema_for_type};
use crate::util::pascal_case;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{Data, Fields, FnArg, Ident, ItemFn, LitBool, LitStr, Type, spanned::Spanned};

pub(crate) fn expand_tool(input: &syn::DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let struct_name = &input.ident;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            Fields::Unit => {
                // No parameters: still emit an (empty-object) spec.
                return Ok(tool_spec_impl(
                    struct_name,
                    &tool_meta(input)?,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    input,
                    "derive(Tool) requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "derive(Tool) can only be applied to structs",
            ));
        }
    };

    let meta = tool_meta(input)?;

    let mut property_inserts = Vec::new();
    let mut required = Vec::new();

    for field in fields {
        let ident = field.ident.as_ref().ok_or_else(|| {
            syn::Error::new_spanned(field, "derive(Tool) requires every field to be named")
        })?;
        let name = ident.to_string();

        let default = field_default(field)?;
        let description = doc_string(&field.attrs);

        let (schema_expr, optional) =
            schema_for_type(&field.ty, description.as_deref(), default.as_ref())?;

        property_inserts.push(quote! {
            __props.insert(#name.to_string(), #schema_expr);
        });

        if !optional && default.is_none() {
            required.push(name);
        }
    }

    Ok(tool_spec_impl(
        struct_name,
        &meta,
        property_inserts,
        required,
    ))
}

struct ToolMeta {
    name: String,
    description: String,
}

/// Read the struct-level `#[tool(name = ..., description = ...)]` attribute.
fn tool_meta(input: &syn::DeriveInput) -> syn::Result<ToolMeta> {
    let mut name = input.ident.to_string();
    let mut description = String::new();

    for attr in &input.attrs {
        if !attr.path().is_ident("tool") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let lit: LitStr = meta.value()?.parse()?;
                name = lit.value();
                Ok(())
            } else if meta.path.is_ident("description") {
                let lit: LitStr = meta.value()?.parse()?;
                description = lit.value();
                Ok(())
            } else {
                Err(meta
                    .error("unsupported `tool` attribute key (expected `name` or `description`)"))
            }
        })?;
    }

    Ok(ToolMeta { name, description })
}

/// Emit the `impl <Struct> { pub fn tool_spec() -> ToolSpec { ... } }`.
fn tool_spec_impl(
    struct_name: &Ident,
    meta: &ToolMeta,
    property_inserts: Vec<proc_macro2::TokenStream>,
    required: Vec<String>,
) -> proc_macro2::TokenStream {
    let name = &meta.name;
    let description = &meta.description;

    let required_tokens = if required.is_empty() {
        quote! { ::core::option::Option::None }
    } else {
        quote! { ::core::option::Option::Some(::std::vec![ #( #required.to_string() ),* ]) }
    };

    let properties_tokens = if property_inserts.is_empty() {
        quote! { ::core::option::Option::Some(::std::collections::BTreeMap::new()) }
    } else {
        quote! {
            ::core::option::Option::Some({
                let mut __props: ::std::collections::BTreeMap<
                    ::std::string::String,
                    ::harw_tools::JsonSchema,
                > = ::std::collections::BTreeMap::new();
                #( #property_inserts )*
                __props
            })
        }
    };

    quote! {
        impl #struct_name {
            /// JSON-Schema-backed tool specification generated by `#[derive(Tool)]`.
            pub fn tool_spec() -> ::harw_tools::ToolSpec {
                let parameters = ::harw_tools::JsonSchema {
                    schema_type: ::core::option::Option::Some(
                        ::harw_tools::JsonSchemaType::Object,
                    ),
                    properties: #properties_tokens,
                    required: #required_tokens,
                    ..::core::default::Default::default()
                };

                ::harw_tools::ToolSpec::Function(::harw_tools::FunctionToolSpec {
                    name: ::harw_tools::ToolName::new(#name),
                    description: #description.to_string(),
                    parameters,
                    strict: false,
                })
            }
        }
    }
}

/// Erlaubte `permission = "..."`-Werte und die zugehörige Variante von
/// `harw_authority::Permission` (re-exportiert als `::harw_tools::Permission`).
///
/// Die Tabelle ist bewusst die *einzige* Stelle, an der ein String auf eine
/// Berechtigung abgebildet wird: ein unbekannter Wert wird abgelehnt statt
/// stillschweigend auf eine andere Variante zu fallen.
const PERMISSION_VALUES: &[(&str, &str)] = &[
    ("read_workspace", "ReadWorkspace"),
    ("write_workspace", "WriteWorkspace"),
    ("execute_process", "ExecuteProcess"),
    ("network_access", "NetworkAccess"),
    ("read_secrets", "ReadSecrets"),
    ("manage_plugins", "ManagePlugins"),
    ("read_cargo_registry", "ReadCargoRegistry"),
];

/// Der Permission-Wert, der Netzzugriff bedeutet; `host_from` ist nur in
/// Kombination mit diesem Wert sinnvoll.
const NETWORK_PERMISSION: &str = "network_access";

// Kommaseparierte Liste der erlaubten Permission-Strings für Fehlermeldungen.
fn allowed_permission_list() -> String {
    PERMISSION_VALUES
        .iter()
        .map(|(key, _)| format!("\"{key}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Geparste Argumente des `#[tool(...)]`-Attributs.
///
/// Die Felder bleiben modul-privat; `lib.rs` reicht den Wert nur an
/// [`expand_tool_fn`] weiter.
#[derive(Debug)]
pub(crate) struct ToolAttr {
    /// `name = "..."` — überschreibt den aus dem Funktionsnamen abgeleiteten Tool-Namen.
    name: Option<String>,
    /// `description = "..."` — Beschreibung für Modell und `ToolSpec`.
    description: String,
    /// `permission = "..."` — bereits gegen [`PERMISSION_VALUES`] validiert.
    permission: Option<LitStr>,
    /// `host_from = "..."` — Feldname im Args-Struct, aus dem der Host gezogen wird.
    host_from: Option<LitStr>,
    /// `parallel_safe` (Flag) oder `parallel_safe = <bool>`; Default `false`.
    parallel_safe: bool,
}

/// Liest den rohen `#[tool(...)]`-Attribut-Token-Stream in [`ToolAttr`] ein.
///
/// # Unterstützte Schlüssel
/// - `name = "..."`, `description = "..."` (unverändert zum bisherigen Verhalten)
/// - `permission = "<wert>"` — einer der Schlüssel aus [`PERMISSION_VALUES`]
/// - `host_from = "<feldname>"`
/// - `parallel_safe` als Flag oder `parallel_safe = true|false`
///
/// # Errors
/// - unbekannter Attribut-Schlüssel,
/// - unbekannter `permission`-Wert (Fehlermeldung listet die erlaubten Werte),
/// - `host_from` ohne `permission = "network_access"` — der Host-Check ohne
///   Netz-Berechtigung wäre eine halbe Prüfung und wird deshalb abgelehnt.
pub(crate) fn parse_tool_attr(attr: proc_macro2::TokenStream) -> syn::Result<ToolAttr> {
    let mut name: Option<String> = None;
    let mut description = String::new();
    let mut permission: Option<LitStr> = None;
    let mut host_from: Option<LitStr> = None;
    let mut parallel_safe = false;

    let attr_parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") {
            let lit: LitStr = meta.value()?.parse()?;
            name = Some(lit.value());
            Ok(())
        } else if meta.path.is_ident("description") {
            let lit: LitStr = meta.value()?.parse()?;
            description = lit.value();
            Ok(())
        } else if meta.path.is_ident("permission") {
            let lit: LitStr = meta.value()?.parse()?;
            permission = Some(lit);
            Ok(())
        } else if meta.path.is_ident("host_from") {
            let lit: LitStr = meta.value()?.parse()?;
            host_from = Some(lit);
            Ok(())
        } else if meta.path.is_ident("parallel_safe") {
            // Sowohl `parallel_safe` (Flag) als auch `parallel_safe = true|false`.
            if meta.input.peek(syn::Token![=]) {
                let lit: LitBool = meta.value()?.parse()?;
                parallel_safe = lit.value();
            } else {
                parallel_safe = true;
            }
            Ok(())
        } else {
            Err(meta.error(
                "unsupported `tool` attribute key (expected `name`, `description`, \
                 `permission`, `host_from`, or `parallel_safe`)",
            ))
        }
    });
    attr_parser.parse2(attr)?;

    // Permission-String sofort validieren, damit der Fehler am Literal hängt
    // und nicht erst im generierten Code auftaucht.
    if let Some(lit) = permission.as_ref() {
        permission_variant(lit)?;
    }

    // `host_from` ohne Netz-Permission: der Host-Check allein sagt nichts über
    // die Berechtigung aus, Netzzugriff überhaupt zu eröffnen.
    if let Some(lit) = host_from.as_ref() {
        let is_network = permission
            .as_ref()
            .is_some_and(|perm| perm.value() == NETWORK_PERMISSION);
        if !is_network {
            return Err(syn::Error::new(
                lit.span(),
                format!(
                    "`host_from` requires `permission = \"{NETWORK_PERMISSION}\"` on the same \
                     `#[tool]` attribute; a host check without the network permission would \
                     only be half a guard"
                ),
            ));
        }
        // Feldname muss ein gültiger Bezeichner sein, sonst panickt `Ident::new`.
        field_ident(lit)?;
    }

    Ok(ToolAttr {
        name,
        description,
        permission,
        host_from,
        parallel_safe,
    })
}

/// Übersetzt einen `permission`-String in den Bezeichner der zugehörigen
/// `Permission`-Variante.
fn permission_variant(lit: &LitStr) -> syn::Result<Ident> {
    let value = lit.value();

    if let Some((_, variant)) = PERMISSION_VALUES.iter().find(|(key, _)| *key == value) {
        return Ok(Ident::new(variant, lit.span()));
    }

    // `read_cargo_registry` ist seit W1-18 eine echte `harw_authority::Permission`-
    // Variante und wird oben regulär aufgelöst; die frühere Sonderdiagnose
    // entfällt damit.
    Err(syn::Error::new(
        lit.span(),
        format!(
            "unknown `permission` value \"{value}\" (allowed: {})",
            allowed_permission_list()
        ),
    ))
}

/// Übersetzt einen `host_from`-String in einen Feld-Bezeichner mit der Span des
/// Literals, damit Fehler auf das Attribut zeigen.
fn field_ident(lit: &LitStr) -> syn::Result<Ident> {
    let value = lit.value();
    let mut ident = syn::parse_str::<Ident>(&value).map_err(|_| {
        syn::Error::new(
            lit.span(),
            format!("`host_from = \"{value}\"` is not a valid field identifier"),
        )
    })?;
    ident.set_span(lit.span());
    Ok(ident)
}

pub(crate) fn expand_tool_fn(
    func: ItemFn,
    attr: ToolAttr,
) -> syn::Result<proc_macro2::TokenStream> {
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[tool] requires an `async fn`",
        ));
    }

    let mut arguments = func.sig.inputs.iter();
    let context_argument = arguments.next().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "#[tool] requires `(context: &ToolExecutionContext, args: Args)`",
        )
    })?;
    let args_argument = arguments.next().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "#[tool] requires `(context: &ToolExecutionContext, args: Args)`",
        )
    })?;
    if arguments.next().is_some() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[tool] accepts exactly `(context: &ToolExecutionContext, args: Args)`",
        ));
    }
    let has_context_reference = matches!(context_argument, FnArg::Typed(argument)
        if matches!(argument.ty.as_ref(), Type::Reference(_)));
    if !has_context_reference {
        return Err(syn::Error::new_spanned(
            context_argument,
            "the first #[tool] argument must be `&ToolExecutionContext`",
        ));
    }
    let FnArg::Typed(args_pattern) = args_argument else {
        return Err(syn::Error::new_spanned(
            args_argument,
            "the second #[tool] argument must be deserializable arguments",
        ));
    };
    let args_type = args_pattern.ty.as_ref();

    let fn_ident = func.sig.ident.clone();
    let fn_name = fn_ident.to_string();
    let wrapper_ident = format_ident!("{}Tool", pascal_case(&fn_name));

    let ToolAttr {
        name,
        description,
        permission,
        host_from,
        parallel_safe,
    } = attr;
    let tool_name = name.unwrap_or_else(|| fn_name.clone());

    // `PERMISSION` ist die auditierbare Deklaration; der Prolog unten ist die
    // tatsächliche Durchsetzung. Beide stammen aus demselben Attribut-Literal.
    let (permission_const, permission_prologue) = match permission.as_ref() {
        Some(lit) => {
            let variant = permission_variant(lit)?;
            (
                quote! {
                    ::core::option::Option::Some(::harw_tools::Permission::#variant)
                },
                quote! {
                    if let ::core::option::Option::Some(__denied) = ::harw_tools::require_permission(
                        context,
                        ::harw_tools::Permission::#variant,
                        Self::NAME,
                    ) {
                        return ::core::result::Result::Ok(__denied);
                    }
                },
            )
        }
        None => (
            quote! { ::core::option::Option::None },
            proc_macro2::TokenStream::new(),
        ),
    };

    // Host-Prüfung erst nach der Deserialisierung: der Host steckt in den
    // Argumenten. Ein nicht extrahierbarer Host bricht ab (fail closed) statt
    // einen leeren Hostnamen in die Scope-Prüfung zu geben.
    let host_prologue = match host_from.as_ref() {
        Some(lit) => {
            let field = field_ident(lit)?;
            let field_name = lit.value();
            quote! {
                let __host = match ::harw_tools::host_from_url(&args.#field) {
                    ::core::option::Option::Some(__host) => __host,
                    ::core::option::Option::None => {
                        return ::core::result::Result::Ok(::harw_tools::ToolOutput::error(
                            ::std::format!(
                                "Tool '{}': Feld '{}' enthält keinen extrahierbaren Hostnamen",
                                Self::NAME,
                                #field_name,
                            ),
                        ));
                    }
                };
                if let ::core::option::Option::Some(__denied) = ::harw_tools::require_host_access(
                    context,
                    &__host,
                    Self::NAME,
                ) {
                    return ::core::result::Result::Ok(__denied);
                }
            }
        }
        None => proc_macro2::TokenStream::new(),
    };

    Ok(quote! {
        #func

        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy, ::core::default::Default)]
        pub struct #wrapper_ident;

        impl #wrapper_ident {
            pub const NAME: &'static str = #tool_name;
            pub const DESCRIPTION: &'static str = #description;
            /// Ob unabhängige Aufrufe dieses Tools nebenläufig laufen dürfen.
            /// Default `false` — Seiteneffekte gelten als nicht kommutativ,
            /// solange das Tool nichts anderes erklärt.
            pub const PARALLEL_SAFE: bool = #parallel_safe;
            /// Die vom generierten Prolog geprüfte Berechtigung, als
            /// auditierbare Deklaration. `None` bedeutet: dieses Tool erzwingt
            /// keine Berechtigung im Makro-Prolog.
            pub const PERMISSION: ::core::option::Option<::harw_tools::Permission> =
                #permission_const;

            /// Die `ToolSpec` dieses Tools, abgeleitet aus dem Args-Struct.
            ///
            /// Name und Beschreibung werden bewusst aus `NAME`/`DESCRIPTION`
            /// überschrieben: `tools()` und `executor(name)` müssen denselben
            /// Namen verwenden, sonst bewirbt ein Provider ein Tool, das er
            /// anschließend nicht auflösen kann.
            #[must_use]
            pub fn spec() -> ::harw_tools::ToolSpec {
                match <#args_type>::tool_spec() {
                    ::harw_tools::ToolSpec::Function(mut __function) => {
                        __function.name = ::harw_tools::ToolName::new(Self::NAME);
                        __function.description =
                            ::std::string::ToString::to_string(Self::DESCRIPTION);
                        ::harw_tools::ToolSpec::Function(__function)
                    }
                }
            }
        }

        impl ::harw_tools::ToolExecutor for #wrapper_ident {
            fn execute<'a>(
                &'a self,
                context: &'a ::harw_tools::ToolExecutionContext,
                call: &'a ::harw_tools::ToolCall,
            ) -> ::harw_tools::ToolExecutorFuture<'a> {
                ::std::boxed::Box::pin(async move {
                    // Autorität zuerst: vor jeder Verarbeitung modell-kontrollierter Argumente.
                    #permission_prologue
                    let args: #args_type = ::serde_json::from_value(call.arguments.clone())?;
                    #host_prologue
                    #fn_ident(context, args).await
                })
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use syn::DeriveInput;

    /// Erzeugt eine `#[tool]`-taugliche Beispielfunktion für Expansionstests.
    fn sample_fn() -> ItemFn {
        syn::parse_quote! {
            async fn fetch_page(
                context: &ToolExecutionContext,
                args: FetchPageArgs,
            ) -> Result<ToolOutput, ToolsError> {
                let _ = (context, args);
                Ok(ToolOutput::text("ok"))
            }
        }
    }

    // ---------------------------------------------------------------------
    // parse_tool_attr
    // ---------------------------------------------------------------------

    /// Alle fünf Schlüssel werden gelesen und landen in [`ToolAttr`].
    #[test]
    fn test_parse_tool_attr_reads_all_supported_keys() -> TestResult {
        let attr = parse_tool_attr(quote! {
            name = "http.fetch",
            description = "Fetches a page",
            permission = "network_access",
            host_from = "url",
            parallel_safe,
        })
        .map_err(ctx("a fully populated attribute must parse"))?;

        assert_eq!(attr.name.as_deref(), Some("http.fetch"));
        assert_eq!(attr.description, "Fetches a page");
        assert_eq!(
            attr.permission.as_ref().map(LitStr::value).as_deref(),
            Some("network_access")
        );
        assert_eq!(
            attr.host_from.as_ref().map(LitStr::value).as_deref(),
            Some("url")
        );
        assert!(attr.parallel_safe);
        Ok(())
    }

    /// Ohne Angaben bleiben die sicherheitsrelevanten Felder fail-closed:
    /// keine Permission und `parallel_safe == false`.
    #[test]
    fn test_parse_tool_attr_defaults_are_fail_closed() -> TestResult {
        let attr = parse_tool_attr(quote! { description = "no keys" })
            .map_err(ctx("a minimal attribute must parse"))?;

        assert!(attr.name.is_none());
        assert!(attr.permission.is_none());
        assert!(attr.host_from.is_none());
        assert!(
            !attr.parallel_safe,
            "parallel_safe must default to false so side effects are presumed non-commutative"
        );
        Ok(())
    }

    /// `parallel_safe = false` überschreibt das Flag explizit.
    #[test]
    fn test_parse_tool_attr_parallel_safe_accepts_explicit_bool() -> TestResult {
        let attr = parse_tool_attr(quote! { parallel_safe = false })
            .map_err(ctx("explicit bool form must parse"))?;
        assert!(!attr.parallel_safe);

        let attr = parse_tool_attr(quote! { parallel_safe = true })
            .map_err(ctx("explicit bool form must parse"))?;
        assert!(attr.parallel_safe);
        Ok(())
    }

    /// Jeder Tabellenwert wird akzeptiert und auf die erwartete Variante abgebildet.
    #[test]
    fn test_parse_tool_attr_accepts_every_known_permission() -> TestResult {
        for (key, variant) in PERMISSION_VALUES {
            let literal = LitStr::new(key, proc_macro2::Span::call_site());
            let resolved = permission_variant(&literal)
                .map_err(|_| TestError::Unexpected(format!("permission {key:?} must resolve")))?;
            assert_eq!(resolved.to_string(), *variant);
        }
        Ok(())
    }

    /// Unbekannter Permission-String → Fehler, der die erlaubten Werte auflistet.
    #[test]
    fn test_parse_tool_attr_rejects_unknown_permission() -> TestResult {
        let Err(error) = parse_tool_attr(quote! { permission = "root_access" }) else {
            return Err(TestError::Unexpected(
                "an unknown permission must be a macro diagnostic".to_owned(),
            ));
        };
        let message = error.to_string();

        assert!(
            message.contains("root_access"),
            "message must name the rejected value, got: {message}"
        );
        assert!(
            message.contains("read_workspace") && message.contains("manage_plugins"),
            "message must list the allowed values, got: {message}"
        );
        Ok(())
    }

    /// `read_cargo_registry` ist seit W1-18 eine echte `Permission`-Variante und
    /// muss regulär auf `ReadCargoRegistry` abbilden — Dependency-Quellen-Tools
    /// hängen daran.
    #[test]
    fn test_parse_tool_attr_accepts_read_cargo_registry() -> TestResult {
        let parsed = parse_tool_attr(quote! { permission = "read_cargo_registry" }).map_err(
            ctx("read_cargo_registry must resolve to a Permission variant"),
        )?;
        let permission = parsed
            .permission
            .as_ref()
            .ok_or(TestError::Missing("permission must be present"))?;
        assert_eq!(
            permission_variant(permission)
                .map_err(ctx("variant lookup must succeed"))?
                .to_string(),
            "ReadCargoRegistry"
        );
        Ok(())
    }

    /// `host_from` ohne `permission = "network_access"` ist nur eine halbe Prüfung.
    #[test]
    fn test_parse_tool_attr_rejects_host_from_without_network_access() -> TestResult {
        let Err(error) = parse_tool_attr(quote! {
            permission = "read_workspace",
            host_from = "url",
        }) else {
            return Err(TestError::Unexpected(
                "host_from without the network permission must be rejected".to_owned(),
            ));
        };
        let message = error.to_string();

        assert!(
            message.contains("network_access"),
            "message must name the required permission, got: {message}"
        );
        Ok(())
    }

    /// Auch ganz ohne `permission` ist `host_from` unzulässig.
    #[test]
    fn test_parse_tool_attr_rejects_host_from_without_any_permission() -> TestResult {
        let Err(error) = parse_tool_attr(quote! { host_from = "url" }) else {
            return Err(TestError::Unexpected(
                "host_from alone must be rejected".to_owned(),
            ));
        };
        assert!(error.to_string().contains("network_access"));
        Ok(())
    }

    /// Kein gültiger Bezeichner → Fehler statt Panik in `Ident::new`.
    #[test]
    fn test_parse_tool_attr_rejects_non_identifier_host_from() -> TestResult {
        let Err(error) = parse_tool_attr(quote! {
            permission = "network_access",
            host_from = "not a field",
        }) else {
            return Err(TestError::Unexpected(
                "a non-identifier field name must be rejected".to_owned(),
            ));
        };
        assert!(error.to_string().contains("valid field identifier"));
        Ok(())
    }

    /// Unbekannter Schlüssel → Fehler, der die unterstützten Schlüssel nennt.
    #[test]
    fn test_parse_tool_attr_rejects_unknown_key() -> TestResult {
        let Err(error) = parse_tool_attr(quote! { retries = 3 }) else {
            return Err(TestError::Unexpected(
                "an unknown key must be a macro diagnostic".to_owned(),
            ));
        };
        let message = error.to_string();

        assert!(
            message.contains("host_from") && message.contains("parallel_safe"),
            "message must list the supported keys, got: {message}"
        );
        Ok(())
    }

    // ---------------------------------------------------------------------
    // expand_tool_fn
    // ---------------------------------------------------------------------

    /// Der Permission-Prolog steht vor der Deserialisierung der Argumente.
    #[test]
    fn test_expand_tool_fn_checks_permission_before_deserialization() -> TestResult {
        let attr =
            parse_tool_attr(quote! { permission = "read_workspace" }).map_err(ctx("parses"))?;
        let expanded = expand_tool_fn(sample_fn(), attr)
            .map_err(ctx("a permission-guarded tool must expand"))?
            .to_string();

        let guard = expanded
            .find("require_permission")
            .ok_or(TestError::Missing(
                "expansion must contain the permission prologue",
            ))?;
        let deserialize = expanded.find("from_value").ok_or(TestError::Missing(
            "expansion must still deserialize the arguments",
        ))?;

        assert!(
            guard < deserialize,
            "the permission guard must run before model-controlled arguments are parsed"
        );
        assert!(expanded.contains("Permission :: ReadWorkspace"));
        assert!(expanded.contains("PARALLEL_SAFE : bool = false"));
        Ok(())
    }

    /// Ohne `permission` wird kein Prolog erzeugt und `PERMISSION` ist `None`.
    #[test]
    fn test_expand_tool_fn_without_permission_emits_no_prologue() -> TestResult {
        let attr = parse_tool_attr(quote! { description = "plain" }).map_err(ctx("parses"))?;
        let expanded = expand_tool_fn(sample_fn(), attr)
            .map_err(ctx("an unguarded tool must still expand"))?
            .to_string();

        assert!(!expanded.contains("require_permission"));
        assert!(expanded.contains("PERMISSION : :: core :: option :: Option"));
        assert!(expanded.contains(":: core :: option :: Option :: None"));
        Ok(())
    }

    /// Der Host-Prolog liegt nach der Deserialisierung und bricht bei einem
    /// nicht extrahierbaren Host ab, statt einen leeren Host weiterzureichen.
    #[test]
    fn test_expand_tool_fn_host_prologue_is_fail_closed() -> TestResult {
        let attr = parse_tool_attr(quote! {
            permission = "network_access",
            host_from = "url",
        })
        .map_err(ctx("parses"))?;
        let expanded = expand_tool_fn(sample_fn(), attr)
            .map_err(ctx("a host-guarded tool must expand"))?
            .to_string();

        let deserialize = expanded
            .find("from_value")
            .ok_or(TestError::Missing("deserialization"))?;
        let host = expanded
            .find("host_from_url")
            .ok_or(TestError::Missing("expansion must extract the host"))?;
        let guard = expanded
            .find("require_host_access")
            .ok_or(TestError::Missing("expansion must check host access"))?;

        assert!(deserialize < host, "the host is read out of the arguments");
        assert!(host < guard, "extraction precedes the scope check");
        assert!(
            !expanded.contains("unwrap_or_default"),
            "an unparseable URL must not silently become an empty host"
        );
        assert!(
            expanded.contains("keinen extrahierbaren Hostnamen"),
            "an unparseable URL must return an explicit error"
        );
        assert!(expanded.contains("args . url"));
        Ok(())
    }

    /// `spec()` überschreibt Name und Beschreibung aus den Consts, damit
    /// `tools()` und `executor(name)` nicht auseinanderlaufen können.
    #[test]
    fn test_expand_tool_fn_spec_overrides_name_and_description() -> TestResult {
        let attr = parse_tool_attr(quote! {
            name = "http.fetch",
            description = "Fetches a page",
        })
        .map_err(ctx("parses"))?;
        let expanded = expand_tool_fn(sample_fn(), attr)
            .map_err(ctx("expands"))?
            .to_string();

        assert!(expanded.contains("const NAME"));
        assert!(expanded.contains("\"http.fetch\""));
        assert!(expanded.contains("< FetchPageArgs > :: tool_spec ()"));
        assert!(
            expanded
                .contains("__function . name = :: harw_tools :: ToolName :: new (Self :: NAME)")
        );
        assert!(expanded.contains("__function . description"));
        Ok(())
    }

    /// Nicht-`async fn` bleibt ein Makro-Fehler.
    #[test]
    fn test_expand_tool_fn_rejects_sync_functions() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn fetch_page(context: &ToolExecutionContext, args: FetchPageArgs) -> u8 { 0 }
        };
        let attr = parse_tool_attr(quote! {}).map_err(ctx("parses"))?;
        let Err(error) = expand_tool_fn(func, attr) else {
            return Err(TestError::Unexpected(
                "sync functions must be rejected".to_owned(),
            ));
        };
        assert!(error.to_string().contains("async fn"));
        Ok(())
    }

    #[test]
    fn schema_for_type_emits_supported_literal_defaults() -> TestResult {
        let cases: Vec<(Type, syn::Expr, &str)> = vec![
            (syn::parse_quote!(u8), syn::parse_quote!(10), "json ! (10)"),
            (
                syn::parse_quote!(bool),
                syn::parse_quote!(false),
                "json ! (false)",
            ),
            (
                syn::parse_quote!(Vec<String>),
                syn::parse_quote!(["fast", "safe"]),
                "json ! ([\"fast\" , \"safe\"])",
            ),
        ];

        for (ty, default, expected_default) in cases {
            let (schema, optional) = schema_for_type(&ty, None, Some(&default))
                .map_err(ctx("supported field type must generate a schema"))?;
            let schema = schema.to_string();

            assert!(!optional);
            assert!(schema.contains("default : :: core :: option :: Option :: Some"));
            assert!(schema.contains(":: harw_tools :: serde_json :: json !"));
            assert!(schema.contains(expected_default), "schema was: {schema}");
        }
        Ok(())
    }

    #[test]
    fn option_field_default_is_emitted_and_not_required() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct SearchArgs {
                required: String,
                #[tool(default = 25)]
                page_size: Option<u8>,
            }
        };

        let expanded = expand_tool(&input)
            .map_err(ctx("supported tool arguments must expand"))?
            .to_string();

        assert!(expanded.contains(
            "default : :: core :: option :: Option :: Some (:: harw_tools :: serde_json :: json ! (25))"
        ));
        assert!(expanded.contains(":: std :: vec ! [\"required\" . to_string ()]"));
        Ok(())
    }

    #[test]
    fn rejects_default_expressions_that_cannot_be_serialized_safely() -> TestResult {
        let field: syn::Field = syn::parse_quote! {
            #[tool(default = DEFAULT_PAGE_SIZE)]
            page_size: u8
        };

        let Err(error) = field_default(&field) else {
            return Err(TestError::Unexpected(
                "non-literal defaults must remain macro diagnostics".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "tool default must be a JSON-compatible literal expression"
        );
        Ok(())
    }
}
