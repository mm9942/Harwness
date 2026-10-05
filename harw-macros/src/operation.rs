//! `#[operation]`-Expansion.
//!
//! Verwandelt eine `async fn` in eine Unit-Struktur plus
//! `impl ::harw_operations::Operation`. `parse_operation_args` liest den rohen
//! Attribut-Token-Stream in [`OperationArgs`] ein (Top-Level-Schlüssel `name`,
//! `summary`, `domain`, `permission`, `aliases`, `category` sowie die
//! verschachtelten Gruppen `command(...)`, `model_tool(...)`,
//! `agent_tool(...)`, `web(...)`); `expand_operation` validiert die Pflichtfelder und
//! erzeugt den Ziel-Code — einschließlich `OperationMeta::args_schema`, das
//! über `harw_operations::operation::ArgsSchemaProbe` **bedingt** an
//! `<ArgsType as OpArgsSchema>::json_schema` gebunden wird (siehe
//! `expand_operation`), sowie `OperationMeta::output_schema`, das derzeit
//! unbedingt auf `None` gesetzt wird (kein Attribut-Schlüssel dafür, Stand
//! W3/C-OPS). `map_domain`, `map_permission`, `map_visibility`, `map_approval`,
//! `map_web_method` und `map_busy_availability` übersetzen die jeweiligen
//! String-Literale in `::harw_operations`-Enum-Varianten — `map_web_method`
//! mappt den verpflichtenden `method`-Schlüssel von `web(...)` auf
//! `harw_operations::operation::WebMethod` (W3/C-OPS, F-031: die HTTP-Methode
//! einer `Surface::Web`-Route wurde zuvor aus `model_tool(readonly)`
//! abgeleitet; `method` macht sie zu einer eigenständigen Pflichtangabe, ein
//! fehlender Schlüssel ist ein Compile-Fehler); `map_busy_availability` mappt
//! den optionalen `busy`-Schlüssel von `command(...)` (Werte `"immediate"` /
//! `"staged"` / `"deferred"`, Default `"deferred"`) auf das TOP-LEVEL-Feld
//! `OperationMeta::busy` (`harw_operations::operation::BusyAvailability`) —
//! nicht auf ein `Surface::Command`-Feld. Der optionale Schlüssel
//! `busy_subcommands = "show=immediate, switch=staged, -=immediate"` von
//! `command(...)` überschreibt die Klasse je Unterbefehl (`-` = bare Form);
//! `parse_busy_subcommands` erzeugt daraus eine Überschreibung von
//! `Operation::busy_subcommands`. Diese Funktionen sind die
//! Einstiegspunkte, die die `#[proc_macro_attribute] operation`-Funktion im
//! Crate-Root (`lib.rs`) aufruft.

use crate::util::pascal_case;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{FnArg, ItemFn, LitStr, Type, spanned::Spanned};

/// Die Autoritäts-Reducer, die eine Laufzeit auflösen kann.
///
/// Muss mit `KNOWN_AUTHORITY_REDUCERS` in `harw-core-bridge/src/agent_tool.rs`
/// übereinstimmen. Die Duplikation ist unvermeidbar — `harw-macros` ist eine
/// proc-macro-Crate und darf nicht auf `harw-core-bridge` zeigen (das würde die
/// Laufzeit in den Compiler-Prozess ziehen).
///
/// **Die Listen sind nicht automatisch gekoppelt.** Wächst die eine, muss die
/// andere von Hand folgen. Was die Divergenz begrenzt: ein neuer Reducer, der
/// nur hier steht, macht `resolve_authority_reducer` still auf `read_only`
/// zurückfallen; einer, der nur dort steht, ist gar nicht deklarierbar — das
/// Makro weist ihn ab. Beide Richtungen scheitern also sichtbar, sobald jemand
/// den Reducer tatsächlich benutzt.
const KNOWN_AUTHORITY_REDUCERS: &[&str] = &[
    "reduce_to_read_only",
    "reduce_to_read_execute",
    "reduce_to_read_registry",
    "reduce_to_read_network",
    "reduce_to_read_explore",
    "reduce_to_read_workspace_network",
];

/// All parsed attribute arguments for `#[operation]`, collected into a single struct
/// so that `expand_operation` does not exceed Clippy's argument-count limit.
///
/// The `has_agent_tool` flag indicates that an `agent_tool(...)` sub-attribute was
/// present. When `true`, all three of `at_child`, `at_authority`, and `at_budget`
/// must be `Some`; `expand_operation` enforces this with a compile-time error.
///
/// # Design-doc reference
/// Spec section: "agent_tool-Attribut" in the `harw-macros` extension brief.
pub(crate) struct OperationArgs {
    op_name: Option<LitStr>,
    op_summary: Option<LitStr>,
    op_domain: Option<LitStr>,
    op_permission: Option<LitStr>,
    has_command: bool,
    cmd_path: Option<LitStr>,
    cmd_visibility: Option<LitStr>,
    /// `busy = "..."` sub-key of `command(...)` → top-level `busy` field of
    /// `OperationMeta` (not a `Surface::Command` field — mapped via
    /// `map_busy_availability`). Absent ⇒ `BusyAvailability::DeferredUntilTurnEnd`
    /// (today's behavior for every command).
    cmd_busy: Option<LitStr>,
    /// `busy_subcommands = "..."` sub-key of `command(...)` → override of
    /// `Operation::busy_subcommands` (see `parse_busy_subcommands`). Absent ⇒
    /// no override (every invocation inherits `busy`).
    cmd_busy_subcommands: Option<LitStr>,
    /// Explizite Remote-Unterbefehle für `visibility = "channel_reduced"`.
    /// `-` bezeichnet die bare Form, `*` alle Argumentformen.
    cmd_channel_subcommands: Option<LitStr>,
    has_model_tool: bool,
    mt_readonly: bool,
    mt_approval: Option<LitStr>,
    /// Whether a `web(...)` sub-attribute was declared.
    ///
    /// # Design-doc reference
    /// `harw-ops`/`harw-operations` UI-05 node: `Surface::Web` existed since
    /// UI-00 but no `#[operation(...)]` invocation could ever declare it — this
    /// is the grammar half of closing that gap. `approval` reuses exactly the
    /// same field as `model_tool(...)` (same meaning, same enum) — deliberately
    /// no third authority axis. `method` (W3/C-OPS, F-031) is `Surface::Web`'s
    /// own mandatory field — it does not exist on `model_tool(...)` and is
    /// never derived from `model_tool(...)`'s `readonly`.
    has_web: bool,
    /// `path = "..."` sub-key of `web(...)` → `path` field of `Surface::Web`.
    /// Mandatory when `web(...)` is present, exactly like `command(...)`'s `path`.
    web_path: Option<LitStr>,
    /// `method = "get"` / `method = "post"` sub-key of `web(...)` →
    /// `method` field of `Surface::Web` (`WebMethod::Get` / `WebMethod::Post`).
    ///
    /// # Design-doc reference
    /// W3/C-OPS, F-031 (`x-findings-register-w1-w3.md`): the HTTP method of a
    /// `Surface::Web` route used to be derived from `model_tool(readonly)`,
    /// which let `analyze` (`harw-ops/src/analyze.rs:879`) expose a mutating
    /// operation on a `GET` route because it declared itself `readonly`.
    /// `method` is now mandatory and independent of `readonly`;
    /// [`expand_operation`] rejects `web(...)` without it at compile time.
    web_method: Option<LitStr>,
    /// `approval = "..."` sub-key of `web(...)` → `approval` field of
    /// `Surface::Web`. Defaults to `ApprovalPolicy::None` when absent, exactly
    /// like `model_tool(...)`.
    web_approval: Option<LitStr>,
    /// Whether an `agent_tool(...)` sub-attribute was declared.
    has_agent_tool: bool,
    /// `child = "..."` sub-key of `agent_tool(...)` → `child_name` field of `Surface::AgentTool`.
    at_child: Option<LitStr>,
    /// `authority = "..."` sub-key of `agent_tool(...)` → `authority_reducer` field.
    at_authority: Option<LitStr>,
    /// `budget = "..."` sub-key of `agent_tool(...)` → `budget_hint` field.
    at_budget: Option<LitStr>,
    /// Optional list of short name aliases (e.g. `aliases = ["r", "reasoning"]`).
    op_aliases: Vec<LitStr>,
    /// Optional category override (e.g. `category = "model"`). Defaults to a
    /// domain-derived value when absent.
    op_category: Option<LitStr>,
}

/// Parse the raw `#[operation(...)]` attribute token stream into [`OperationArgs`].
///
/// # Errors
/// Returns `syn::Error` when an unsupported top-level or nested attribute key
/// appears, mirroring the diagnostics documented on the `#[operation]` entry
/// point in the crate root.
///
/// # Design-doc reference
/// Spec section: "API-Design des Makros" in the `harw-macros` extension brief.
pub(crate) fn parse_operation_args(attr: proc_macro2::TokenStream) -> syn::Result<OperationArgs> {
    let mut op_name: Option<LitStr> = None;
    let mut op_summary: Option<LitStr> = None;
    let mut op_domain: Option<LitStr> = None;
    let mut op_permission: Option<LitStr> = None;
    // command(...) fields
    let mut cmd_path: Option<LitStr> = None;
    let mut cmd_visibility: Option<LitStr> = None;
    let mut cmd_busy: Option<LitStr> = None;
    let mut cmd_busy_subcommands: Option<LitStr> = None;
    let mut cmd_channel_subcommands: Option<LitStr> = None;
    let mut has_command = false;
    // model_tool(...) fields
    let mut mt_readonly = false;
    let mut mt_approval: Option<LitStr> = None;
    let mut has_model_tool = false;
    // web(...) fields
    let mut has_web = false;
    let mut web_path: Option<LitStr> = None;
    let mut web_method: Option<LitStr> = None;
    let mut web_approval: Option<LitStr> = None;
    // agent_tool(...) fields
    let mut at_child: Option<LitStr> = None;
    let mut at_authority: Option<LitStr> = None;
    let mut at_budget: Option<LitStr> = None;
    let mut has_agent_tool = false;
    // optional aliases / category
    let mut op_aliases: Vec<LitStr> = Vec::new();
    let mut op_category: Option<LitStr> = None;

    let attr_parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") {
            let lit: LitStr = meta.value()?.parse()?;
            op_name = Some(lit);
            Ok(())
        } else if meta.path.is_ident("summary") {
            let lit: LitStr = meta.value()?.parse()?;
            op_summary = Some(lit);
            Ok(())
        } else if meta.path.is_ident("domain") {
            let lit: LitStr = meta.value()?.parse()?;
            op_domain = Some(lit);
            Ok(())
        } else if meta.path.is_ident("permission") {
            let lit: LitStr = meta.value()?.parse()?;
            op_permission = Some(lit);
            Ok(())
        } else if meta.path.is_ident("command") {
            has_command = true;
            meta.parse_nested_meta(|nested| {
                if nested.path.is_ident("path") {
                    let lit: LitStr = nested.value()?.parse()?;
                    cmd_path = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("visibility") {
                    let lit: LitStr = nested.value()?.parse()?;
                    cmd_visibility = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("busy") {
                    let lit: LitStr = nested.value()?.parse()?;
                    cmd_busy = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("busy_subcommands") {
                    let lit: LitStr = nested.value()?.parse()?;
                    cmd_busy_subcommands = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("channel_subcommands") {
                    let lit: LitStr = nested.value()?.parse()?;
                    cmd_channel_subcommands = Some(lit);
                    Ok(())
                } else {
                    Err(nested.error(
                        "unsupported `operation` `command` key \
                         (expected `path`, `visibility`, `busy`, `busy_subcommands`, or \
                         `channel_subcommands`)",
                    ))
                }
            })
        } else if meta.path.is_ident("model_tool") {
            has_model_tool = true;
            meta.parse_nested_meta(|nested| {
                if nested.path.is_ident("readonly") {
                    mt_readonly = true;
                    Ok(())
                } else if nested.path.is_ident("approval") {
                    let lit: LitStr = nested.value()?.parse()?;
                    mt_approval = Some(lit);
                    Ok(())
                } else {
                    Err(nested.error(
                        "unsupported `operation` `model_tool` key (expected `readonly` or `approval`)",
                    ))
                }
            })
        } else if meta.path.is_ident("web") {
            has_web = true;
            meta.parse_nested_meta(|nested| {
                if nested.path.is_ident("path") {
                    let lit: LitStr = nested.value()?.parse()?;
                    web_path = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("method") {
                    let lit: LitStr = nested.value()?.parse()?;
                    web_method = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("approval") {
                    let lit: LitStr = nested.value()?.parse()?;
                    web_approval = Some(lit);
                    Ok(())
                } else {
                    Err(nested.error(
                        "unsupported `operation` `web` key (expected `path`, `method`, or `approval`)",
                    ))
                }
            })
        } else if meta.path.is_ident("agent_tool") {
            has_agent_tool = true;
            meta.parse_nested_meta(|nested| {
                if nested.path.is_ident("child") {
                    let lit: LitStr = nested.value()?.parse()?;
                    at_child = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("authority") {
                    let lit: LitStr = nested.value()?.parse()?;
                    at_authority = Some(lit);
                    Ok(())
                } else if nested.path.is_ident("budget") {
                    let lit: LitStr = nested.value()?.parse()?;
                    at_budget = Some(lit);
                    Ok(())
                } else {
                    Err(nested.error(
                        "unsupported `operation` `agent_tool` key \
                         (expected `child`, `authority`, or `budget`)",
                    ))
                }
            })
        } else if meta.path.is_ident("aliases") {
            let arr: syn::ExprArray = meta.value()?.parse()?;
            for elem in arr.elems {
                match elem {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) => op_aliases.push(s),
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "`aliases` elements must be string literals",
                        ));
                    }
                }
            }
            Ok(())
        } else if meta.path.is_ident("category") {
            let lit: LitStr = meta.value()?.parse()?;
            op_category = Some(lit);
            Ok(())
        } else {
            Err(meta.error(
                "unsupported `operation` attribute key \
                 (expected `name`, `summary`, `domain`, `permission`, `command`, \
                 `model_tool`, `web`, `agent_tool`, `aliases`, or `category`)",
            ))
        }
    });
    attr_parser.parse2(attr)?;

    Ok(OperationArgs {
        op_name,
        op_summary,
        op_domain,
        op_permission,
        has_command,
        cmd_path,
        cmd_visibility,
        cmd_busy,
        cmd_busy_subcommands,
        cmd_channel_subcommands,
        has_model_tool,
        mt_readonly,
        mt_approval,
        has_web,
        web_path,
        web_method,
        web_approval,
        has_agent_tool,
        at_child,
        at_authority,
        at_budget,
        op_aliases,
        op_category,
    })
}

/// Validate all parsed `#[operation]` fields and emit the full token stream.
///
/// Builds one `Surface` token per declared sub-attribute (`command`, `model_tool`,
/// `web`, `agent_tool`) — an operation may declare any combination of them; each
/// sub-attribute contributes independently to the `surfaces` vec, none replaces
/// another. For `web`, both `path` and `method` are mandatory (`method` since
/// W3/C-OPS, F-031 — see [`map_web_method`]); for `agent_tool`, all three
/// sub-keys (`child`, `authority`, `budget`) are mandatory; missing keys
/// produce a `syn::Error` at compile time.
///
/// # Argument schema
/// `OperationMeta::args_schema` is filled from the operation's arguments type
/// via `harw_operations::operation::ArgsSchemaProbe`. The binding is
/// conditional: it yields `Some(<ArgsType as OpArgsSchema>::json_schema)` when
/// the type implements the trait (normally through `#[derive(OpArgs)]`) and
/// `None` otherwise. Operations whose arguments type carries no schema keep
/// compiling unchanged and keep their previous, name-matched model-tool schema.
///
/// # Design-doc reference
/// Spec section: "Was das Makro erzeugt" and "agent_tool-Attribut".
pub(crate) fn expand_operation(
    func: ItemFn,
    args: OperationArgs,
) -> syn::Result<proc_macro2::TokenStream> {
    let OperationArgs {
        op_name,
        op_summary,
        op_domain,
        op_permission,
        has_command,
        cmd_path,
        cmd_visibility,
        cmd_busy,
        cmd_busy_subcommands,
        cmd_channel_subcommands,
        has_model_tool,
        mt_readonly,
        mt_approval,
        has_web,
        web_path,
        web_method,
        web_approval,
        has_agent_tool,
        at_child,
        at_authority,
        at_budget,
        op_aliases,
        op_category,
    } = args;
    // --- Validate required keys ----------------------------------------------
    let name_lit = op_name.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "`#[operation]` requires `name = \"...\"`",
        )
    })?;
    let summary_lit = op_summary.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "`#[operation]` requires `summary = \"...\"`",
        )
    })?;
    let domain_lit = op_domain.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "`#[operation]` requires `domain = \"...\"`",
        )
    })?;
    let permission_lit = op_permission.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "`#[operation]` requires `permission = \"...\"`",
        )
    })?;

    // --- Map domain string → token stream ------------------------------------
    let domain_tokens = map_domain(&domain_lit)?;

    // --- Map permission string → token stream --------------------------------
    let permission_tokens = map_permission(&permission_lit)?;

    // --- Build aliases token -------------------------------------------------
    let alias_lits = &op_aliases;
    let aliases_tokens = quote! { &[ #( #alias_lits ),* ] };

    // --- Build busy-availability token (top-level `OperationMeta::busy`) -----
    // `busy` is a sub-key of `command(...)` but sets the TOP-LEVEL
    // `OperationMeta::busy` field, not a `Surface::Command` field — see the
    // `cmd_busy` doc comment on `OperationArgs`. Absent ⇒ the default
    // (`DeferredUntilTurnEnd`), exactly like every other optional key here.
    let busy_tokens = cmd_busy
        .as_ref()
        .map(map_busy_availability)
        .transpose()?
        .unwrap_or_else(|| {
            quote! { ::harw_operations::operation::BusyAvailability::DeferredUntilTurnEnd }
        });
    // Optional per-subcommand override (`busy_subcommands = "..."`): emitted
    // as an override of `Operation::busy_subcommands`; absent ⇒ the trait's
    // default (empty table).
    let busy_subcommands_tokens = match cmd_busy_subcommands.as_ref() {
        Some(lit) => {
            let entries = parse_busy_subcommands(lit)?;
            quote! {
                fn busy_subcommands(
                    &self,
                ) -> &'static [::harw_operations::operation::BusySubcommand] {
                    const TABLE: &[::harw_operations::operation::BusySubcommand] = &[
                        #( #entries ),*
                    ];
                    TABLE
                }
            }
        }
        None => quote! {},
    };

    let is_channel_reduced = cmd_visibility
        .as_ref()
        .is_some_and(|visibility| visibility.value() == "channel_reduced");
    if is_channel_reduced && cmd_channel_subcommands.is_none() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`visibility = \"channel_reduced\"` requires `channel_subcommands = \"...\"`",
        ));
    }
    if !is_channel_reduced && cmd_channel_subcommands.is_some() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`channel_subcommands` is only valid with `visibility = \"channel_reduced\"`",
        ));
    }
    let channel_subcommands_tokens = match cmd_channel_subcommands.as_ref() {
        Some(lit) => {
            let entries = parse_channel_subcommands(lit)?;
            quote! {
                fn channel_subcommands(&self) -> &'static [&'static str] {
                    const TABLE: &[&str] = &[ #( #entries ),* ];
                    TABLE
                }
            }
        }
        None => quote! {},
    };

    // --- Build category token (explicit or derived from domain) --------------
    let category_tokens = match op_category.as_ref() {
        Some(lit) => match lit.value().as_str() {
            "model" => quote! { ::harw_operations::OperationCategory::Model },
            "agent" => quote! { ::harw_operations::OperationCategory::Agent },
            "session" => quote! { ::harw_operations::OperationCategory::Session },
            "system" => quote! { ::harw_operations::OperationCategory::System },
            "knowledge" => quote! { ::harw_operations::OperationCategory::Knowledge },
            "misc" => quote! { ::harw_operations::OperationCategory::Misc },
            other => {
                return Err(syn::Error::new_spanned(
                    lit,
                    format!(
                        "unknown `category` value `{other}`; expected one of: model, agent, session, system, knowledge, misc"
                    ),
                ));
            }
        },
        None => match domain_lit.value().as_str() {
            "session" => quote! { ::harw_operations::OperationCategory::Session },
            "agents" => quote! { ::harw_operations::OperationCategory::Agent },
            "execution" => quote! { ::harw_operations::OperationCategory::System },
            "catalog_config" => quote! { ::harw_operations::OperationCategory::Model },
            "knowledge" => quote! { ::harw_operations::OperationCategory::Knowledge },
            // Infrastruktur-Domänen (Masterplan v2 §31) gruppieren unter "System",
            // analog zu `execution`; muss zu `OperationDomain::default_category` passen.
            "identity" | "network" | "security" | "crypto" => {
                quote! { ::harw_operations::OperationCategory::System }
            }
            _ => quote! { ::harw_operations::OperationCategory::Misc },
        },
    };

    // --- Validate async ------------------------------------------------------
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[operation] requires an `async fn`",
        ));
    }

    // --- Validate argument count and types -----------------------------------
    let mut arguments = func.sig.inputs.iter();
    let ctx_arg = arguments.next().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "#[operation] requires `(ctx: &OpContext, args: ArgsType)`",
        )
    })?;
    let args_arg = arguments.next().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "#[operation] requires `(ctx: &OpContext, args: ArgsType)`",
        )
    })?;
    if arguments.next().is_some() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[operation] accepts exactly `(ctx: &OpContext, args: ArgsType)`",
        ));
    }

    // First arg must be a reference type.
    let has_ctx_reference = matches!(ctx_arg, FnArg::Typed(pat)
        if matches!(pat.ty.as_ref(), Type::Reference(_)));
    if !has_ctx_reference {
        return Err(syn::Error::new_spanned(
            ctx_arg,
            "the first `#[operation]` argument must be `&OpContext` (a reference type)",
        ));
    }

    // Second arg must be a plain typed arg (not a receiver, not a reference).
    let args_type = match args_arg {
        FnArg::Typed(pat) => pat.ty.as_ref().clone(),
        _ => {
            return Err(syn::Error::new_spanned(
                args_arg,
                "the second `#[operation]` argument must be a typed, owned arguments value",
            ));
        }
    };

    // --- Bind the argument schema, but only if the type carries one ----------
    //
    // A proc macro sees the *name* of the arguments type, never its trait impls,
    // so it cannot emit `<ArgsType as OpArgsSchema>::json_schema` unconditionally:
    // every operation whose arguments type lacks `#[derive(OpArgs)]` would stop
    // compiling. `harw_operations::operation::ArgsSchemaProbe` resolves this with
    // autoref-based specialization (stable Rust, the technique `anyhow` uses):
    // `DerivedArgsSchema` is implemented for `&ArgsSchemaProbe<T>` and requires
    // `T: OpArgsSchema`, `NoArgsSchema` for `ArgsSchemaProbe<T>` without bounds.
    // Calling through one reference picks the former when the bound holds and
    // falls through to the latter otherwise — both cases compile.
    let args_schema_tokens = quote! {
        {
            #[allow(unused_imports)]
            use ::harw_operations::operation::{DerivedArgsSchema as _, NoArgsSchema as _};
            let probe = ::harw_operations::operation::ArgsSchemaProbe::<#args_type>::new();
            let probe: &::harw_operations::operation::ArgsSchemaProbe<#args_type> = &probe;
            probe.harw_args_schema()
        }
    };

    // --- Build surfaces tokens -----------------------------------------------
    let mut surface_tokens: Vec<proc_macro2::TokenStream> = Vec::new();

    if has_command {
        let path_lit = cmd_path.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`command(...)` requires a `path = \"...\"` key",
            )
        })?;
        let visibility_tokens = cmd_visibility
            .as_ref()
            .map(map_visibility)
            .transpose()?
            .unwrap_or_else(|| {
                quote! { ::harw_operations::CommandVisibility::ChannelParity }
            });
        // path must be &'static str — a string literal satisfies this.
        let path_str = path_lit.value();
        surface_tokens.push(quote! {
            ::harw_operations::Surface::Command {
                path: #path_str,
                visibility: #visibility_tokens,
            }
        });
    }

    if has_model_tool {
        let approval_tokens = mt_approval
            .as_ref()
            .map(map_approval)
            .transpose()?
            .unwrap_or_else(|| quote! { ::harw_operations::ApprovalPolicy::None });
        let readonly_val = mt_readonly;
        surface_tokens.push(quote! {
            ::harw_operations::Surface::ModelTool {
                readonly: #readonly_val,
                approval: #approval_tokens,
            }
        });
    }

    if has_web {
        // `path` is mandatory, exactly like `command(...)`'s `path` — a Web
        // route without a static HTTP path is not expressible.
        let path_lit = web_path.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`web(...)` requires a `path = \"...\"` key",
            )
        })?;
        // `method` is mandatory (W3/C-OPS, F-031): the HTTP method used to be
        // derived from `model_tool(readonly)`, which let a mutating operation
        // ("analyze", `harw-ops/src/analyze.rs:879`) expose itself on a `GET`
        // route just because it declared `readonly`. There is no default —
        // every `web(...)` declaration must name its method explicitly.
        let method_lit = web_method.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`web(...)` requires a `method = \"get\"` or `method = \"post\"` key",
            )
        })?;
        let method_tokens = map_web_method(&method_lit)?;
        let approval_tokens = web_approval
            .as_ref()
            .map(map_approval)
            .transpose()?
            .unwrap_or_else(|| quote! { ::harw_operations::ApprovalPolicy::None });
        let path_str = path_lit.value();
        surface_tokens.push(quote! {
            ::harw_operations::Surface::Web {
                path: #path_str,
                method: #method_tokens,
                approval: #approval_tokens,
            }
        });
    }

    if has_agent_tool {
        // All three sub-keys are mandatory when `agent_tool(...)` is declared.
        let child_lit = at_child.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`agent_tool(...)` erfordert `child = \"...\"`",
            )
        })?;
        let authority_lit = at_authority.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`agent_tool(...)` erfordert `authority = \"...\"`",
            )
        })?;
        let budget_lit = at_budget.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "`agent_tool(...)` erfordert `budget = \"...\"`",
            )
        })?;
        let child_str = child_lit.value();
        let authority_str = authority_lit.value();
        // Der Reducer-Name wird hier zur Compile-Zeit geprüft, obwohl
        // `harw-core-bridge::resolve_authority_reducer` unbekannte Namen zur
        // Laufzeit fail-closed auf `reduce_to_read_only` abbildet.
        //
        // Beide Prüfungen sind nötig, weil sie verschiedene Fehlerrichtungen
        // fangen: der Laufzeit-Rückfall verhindert eine **Rechteausweitung**
        // (unbekannt ⇒ minimale Rechte). Er verhindert aber nicht die andere
        // Richtung — ein Tippfehler in `reduce_to_read_execute` degradiert ein
        // ausführendes Kind still auf read-only, die Operation ist funktionsunfähig,
        // und nirgends erscheint ein Fehler. Genau das fängt dieser Check.
        //
        // Der Laufzeit-Rückfall bleibt trotzdem: er sieht auch Werte, die nicht
        // aus diesem Makro stammen (Extension-Crates, deserialisierte Metadaten).
        if !KNOWN_AUTHORITY_REDUCERS.contains(&authority_str.as_str()) {
            return Err(syn::Error::new(
                authority_lit.span(),
                format!(
                    "unbekannter authority-Reducer `{authority_str}`; erlaubt sind: {}",
                    KNOWN_AUTHORITY_REDUCERS.join(", ")
                ),
            ));
        }
        let budget_str = budget_lit.value();
        surface_tokens.push(quote! {
            ::harw_operations::Surface::AgentTool {
                child_name: #child_str,
                authority_reducer: #authority_str,
                budget_hint: #budget_str,
            }
        });
    }

    // --- Derive identifiers --------------------------------------------------
    let fn_ident = func.sig.ident.clone();
    let fn_name_str = fn_ident.to_string();
    let struct_ident = format_ident!("{}Operation", pascal_case(&fn_name_str));

    // Extract static string values for the meta.
    let name_str = name_lit.value();
    let summary_str = summary_lit.value();

    // --- Emit tokens ---------------------------------------------------------
    Ok(quote! {
        #func

        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::marker::Copy,
            ::core::default::Default,
        )]
        pub struct #struct_ident;

        impl ::harw_operations::Operation for #struct_ident {
            fn meta(&self) -> &::harw_operations::OperationMeta {
                static META: ::std::sync::OnceLock<::harw_operations::OperationMeta> =
                    ::std::sync::OnceLock::new();
                META.get_or_init(|| ::harw_operations::OperationMeta {
                    name: #name_str,
                    summary: #summary_str,
                    domain: #domain_tokens,
                    permission: #permission_tokens,
                    surfaces: ::std::vec![
                        #( #surface_tokens ),*
                    ],
                    aliases: #aliases_tokens,
                    category: #category_tokens,
                    args_schema: #args_schema_tokens,
                    // W3/C-OPS: there is (yet) no `#[operation(...)]` sub-key
                    // through which an operation declares the shape of its
                    // structured `OpOutput::data`; a future wave can bind
                    // this the same way `args_schema` is bound above, without
                    // breaking this contract again.
                    output_schema: ::core::option::Option::None,
                    busy: #busy_tokens,
                })
            }

            #busy_subcommands_tokens
            #channel_subcommands_tokens

            fn run<'a>(
                &'a self,
                ctx: &'a ::harw_operations::OpContext,
                input: ::harw_operations::OpInput,
            ) -> ::harw_operations::OpFuture<'a> {
                ::std::boxed::Box::pin(async move {
                    // Flächenspezifisches Argument-Parsing (OpInvocation-Sum-Type):
                    // Command → typisiertes `FromRawArgs`, Tool-Flächen → JSON.
                    let args: #args_type = match &input.invocation {
                        ::harw_operations::OpInvocation::Command { args, .. } => {
                            <#args_type as ::harw_operations::FromRawArgs>::from_raw_args(args)?
                        }
                        ::harw_operations::OpInvocation::ModelTool { args, .. }
                        | ::harw_operations::OpInvocation::AgentTool { args, .. } => {
                            if args.is_null() {
                                <#args_type as ::core::default::Default>::default()
                            } else {
                                ::serde_json::from_value(args.clone()).map_err(|e| {
                                    ::harw_operations::OpError::InvalidArguments(
                                        ::std::format!(
                                            "JSON-Deserialisierung fehlgeschlagen: {e}"
                                        ),
                                    )
                                })?
                            }
                        }
                    };
                    #fn_ident(ctx, args).await
                })
            }
        }
    })
}

/// Map a `domain` string literal to its `::harw_operations::OperationDomain` variant tokens.
///
/// # Errors
/// Returns `syn::Error` when the string does not match a known variant.
///
/// # Design-doc reference
/// Spec section: "Domain-Mapping".
fn map_domain(lit: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    match lit.value().as_str() {
        "session" => Ok(quote! { ::harw_operations::OperationDomain::Session }),
        "agents" => Ok(quote! { ::harw_operations::OperationDomain::Agents }),
        "execution" => Ok(quote! { ::harw_operations::OperationDomain::Execution }),
        "catalog_config" => Ok(quote! { ::harw_operations::OperationDomain::CatalogConfig }),
        "knowledge" => Ok(quote! { ::harw_operations::OperationDomain::Knowledge }),
        "identity" => Ok(quote! { ::harw_operations::OperationDomain::Identity }),
        "network" => Ok(quote! { ::harw_operations::OperationDomain::Network }),
        "security" => Ok(quote! { ::harw_operations::OperationDomain::Security }),
        "crypto" => Ok(quote! { ::harw_operations::OperationDomain::Crypto }),
        "misc" => Ok(quote! { ::harw_operations::OperationDomain::Misc }),
        other => Err(syn::Error::new_spanned(
            lit,
            format!(
                "unknown `domain` value `{other}`; \
                 expected one of: session, agents, execution, catalog_config, knowledge, \
                 identity, network, security, crypto, misc"
            ),
        )),
    }
}

/// Map a `permission` string literal to its `::harw_operations::PermissionTier` variant tokens.
///
/// # Errors
/// Returns `syn::Error` when the string does not match a known variant.
///
/// # Design-doc reference
/// Spec section: "Permission-Mapping".
fn map_permission(lit: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    match lit.value().as_str() {
        "observer" => Ok(quote! { ::harw_operations::PermissionTier::Observer }),
        "operator" => Ok(quote! { ::harw_operations::PermissionTier::Operator }),
        "maintainer" => Ok(quote! { ::harw_operations::PermissionTier::Maintainer }),
        "owner" => Ok(quote! { ::harw_operations::PermissionTier::Owner }),
        other => Err(syn::Error::new_spanned(
            lit,
            format!(
                "unknown `permission` value `{other}`; \
                 expected one of: observer, operator, maintainer, owner"
            ),
        )),
    }
}

/// Map a `visibility` string literal to its `::harw_operations::CommandVisibility` variant tokens.
///
/// # Errors
/// Returns `syn::Error` when the string does not match a known variant.
///
/// # Design-doc reference
/// Spec section: "Visibility-Mapping".
fn map_visibility(lit: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    match lit.value().as_str() {
        "tui_only" => Ok(quote! { ::harw_operations::CommandVisibility::TuiOnly }),
        "channel_parity" => Ok(quote! { ::harw_operations::CommandVisibility::ChannelParity }),
        "channel_reduced" => Ok(quote! { ::harw_operations::CommandVisibility::ChannelReduced }),
        other => Err(syn::Error::new_spanned(
            lit,
            format!(
                "unknown `visibility` value `{other}`; \
                 expected one of: tui_only, channel_parity, channel_reduced"
            ),
        )),
    }
}

/// Map a `command(...)` `busy` string literal to its
/// `::harw_operations::operation::BusyAvailability` variant tokens.
///
/// The emitted tokens qualify through the `operation` module — the same
/// pattern this file already uses for `WebMethod`.
///
/// # Errors
/// Returns `syn::Error` when the string is not `"immediate"`, `"staged"` or
/// `"deferred"`.
fn map_busy_availability(lit: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    busy_class_tokens(&lit.value()).ok_or_else(|| {
        syn::Error::new_spanned(
            lit,
            format!(
                "busy muss 'immediate', 'staged' oder 'deferred' sein, war: '{}'",
                lit.value()
            ),
        )
    })
}

/// Tokens der `BusyAvailability`-Variante für einen Klassennamen.
///
/// # Rückgabe
/// `None` für einen unbekannten Namen.
fn busy_class_tokens(value: &str) -> Option<proc_macro2::TokenStream> {
    match value {
        "immediate" => Some(quote! { ::harw_operations::operation::BusyAvailability::Immediate }),
        "staged" => Some(quote! { ::harw_operations::operation::BusyAvailability::Staged }),
        "deferred" => {
            Some(quote! { ::harw_operations::operation::BusyAvailability::DeferredUntilTurnEnd })
        }
        _ => None,
    }
}

/// Parse `busy_subcommands = "show=immediate, switch=staged, -=immediate"`
/// into `BusySubcommand::new(..)` constructor tokens.
///
/// # Beschreibung
/// Einträge sind komma-getrennt, je `unterbefehl=klasse`; `-` steht für die
/// bare Form (kein Argument). Leerraum um Einträge wird ignoriert.
///
/// # Errors
/// `syn::Error` bei leerer Liste, fehlendem `=`, leerem Namen, unbekannter
/// Klasse oder doppeltem Unterbefehl.
fn parse_channel_subcommands(lit: &LitStr) -> syn::Result<Vec<LitStr>> {
    let value = lit.value();
    let mut seen: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    for raw in value.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if name.chars().any(char::is_whitespace) || name.contains('=') {
            return Err(syn::Error::new_spanned(
                lit,
                format!("channel_subcommands: ungültiger Unterbefehl '{name}'"),
            ));
        }
        if seen.iter().any(|prior| prior == name) {
            return Err(syn::Error::new_spanned(
                lit,
                format!("channel_subcommands: Unterbefehl '{name}' doppelt"),
            ));
        }
        seen.push(name.to_owned());
        entries.push(LitStr::new(name, lit.span()));
    }
    if entries.is_empty() {
        return Err(syn::Error::new_spanned(
            lit,
            "channel_subcommands: mindestens ein Unterbefehl, '-' oder '*' nötig",
        ));
    }
    Ok(entries)
}

fn parse_busy_subcommands(lit: &LitStr) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    let value = lit.value();
    let mut seen: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    for raw in value.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Some((name, class)) = raw.split_once('=') else {
            return Err(syn::Error::new_spanned(
                lit,
                format!("busy_subcommands: Eintrag '{raw}' braucht die Form 'unterbefehl=klasse'"),
            ));
        };
        let (name, class) = (name.trim(), class.trim());
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(syn::Error::new_spanned(
                lit,
                format!("busy_subcommands: ungültiger Unterbefehl in '{raw}'"),
            ));
        }
        if seen.iter().any(|prior| prior == name) {
            return Err(syn::Error::new_spanned(
                lit,
                format!("busy_subcommands: Unterbefehl '{name}' doppelt"),
            ));
        }
        seen.push(name.to_owned());
        let class_tokens = busy_class_tokens(class).ok_or_else(|| {
            syn::Error::new_spanned(
                lit,
                format!(
                    "busy_subcommands: Klasse muss 'immediate', 'staged' oder 'deferred' sein, \
                     war: '{class}'"
                ),
            )
        })?;
        let sub_tokens = if name == "-" {
            quote! { ::core::option::Option::None }
        } else {
            quote! { ::core::option::Option::Some(#name) }
        };
        entries.push(quote! {
            ::harw_operations::operation::BusySubcommand::new(#sub_tokens, #class_tokens)
        });
    }
    if entries.is_empty() {
        return Err(syn::Error::new_spanned(
            lit,
            "busy_subcommands: mindestens ein Eintrag 'unterbefehl=klasse' nötig",
        ));
    }
    Ok(entries)
}

/// Map an `approval` string literal to its `::harw_operations::ApprovalPolicy` variant tokens.
///
/// # Errors
/// Returns `syn::Error` when the string does not match a known variant.
///
/// # Design-doc reference
/// Spec section: "Approval-Mapping".
fn map_approval(lit: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    match lit.value().as_str() {
        "none" => Ok(quote! { ::harw_operations::ApprovalPolicy::None }),
        "always" => Ok(quote! { ::harw_operations::ApprovalPolicy::Always }),
        other => Err(syn::Error::new_spanned(
            lit,
            format!("unknown `approval` value `{other}`; expected one of: none, always"),
        )),
    }
}

/// Map a `web(...)` `method` string literal to its
/// `::harw_operations::operation::WebMethod` variant tokens.
///
/// `WebMethod` is not re-exported at the `harw_operations` crate root (as
/// `ApprovalPolicy`/`Surface` are), so the emitted tokens qualify through the
/// `operation` module — the same pattern this file already uses for
/// `ArgsSchemaProbe`.
///
/// # Errors
/// Returns `syn::Error` when the string does not match a known variant.
///
/// # Design-doc reference
/// W3/C-OPS, F-031 (`x-findings-register-w1-w3.md`): `method` replaces the
/// former derivation of a `Surface::Web` route's HTTP method from
/// `model_tool(readonly)`.
fn map_web_method(lit: &LitStr) -> syn::Result<proc_macro2::TokenStream> {
    match lit.value().as_str() {
        "get" => Ok(quote! { ::harw_operations::operation::WebMethod::Get }),
        "post" => Ok(quote! { ::harw_operations::operation::WebMethod::Post }),
        other => Err(syn::Error::new_spanned(
            lit,
            format!("unknown `method` value `{other}`; expected one of: get, post"),
        )),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod operation_tests {
    use super::{expand_operation, map_domain, parse_operation_args};
    use crate::test_support::{TestError, TestResult, ctx};
    use quote::quote;
    use syn::ItemFn;

    /// Entfernt alle Whitespaces aus `TokenStream::to_string()`, damit
    /// Substring-Prüfungen unabhängig von `quote`s Abstandsregeln sind.
    fn normalize(ts: &proc_macro2::TokenStream) -> String {
        ts.to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    /// Expandiert eine minimale Operation mit dem übergebenen Argument-Typ und
    /// gibt den normalisierten Token-Strom zurück.
    fn expand_with_args_type(args_type: proc_macro2::TokenStream) -> TestResult<String> {
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: #args_type) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            model_tool(readonly)
        };
        let args = parse_operation_args(attr)
            .map_err(ctx("die Attribut-Schlüssel sind gültig und müssen parsen"))?;
        let tokens =
            expand_operation(func, args).map_err(ctx("eine gültige Operation muss expandieren"))?;
        Ok(normalize(&tokens))
    }

    #[test]
    fn expand_operation_emits_args_schema_field() -> TestResult {
        let flat = expand_with_args_type(quote!(DemoArgs))?;

        assert!(
            flat.contains("args_schema:"),
            "das erzeugte OperationMeta muss das Feld `args_schema` setzen"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_binds_args_schema_through_the_probe() -> TestResult {
        let flat = expand_with_args_type(quote!(DemoArgs))?;

        assert!(
            flat.contains("::harw_operations::operation::ArgsSchemaProbe::<DemoArgs>::new()"),
            "die Sonde muss auf den deklarierten Argument-Typ instanziiert werden"
        );
        assert!(
            flat.contains("probe.harw_args_schema()"),
            "das Schema muss über die Sonde aufgelöst werden"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_imports_both_probe_branches() -> TestResult {
        let flat = expand_with_args_type(quote!(DemoArgs))?;

        assert!(
            flat.contains("DerivedArgsSchemaas_"),
            "der spezialisierte Zweig muss im Geltungsbereich stehen"
        );
        assert!(
            flat.contains("NoArgsSchemaas_"),
            "ohne den Rückfall-Zweig bräche jeder Args-Typ ohne OpArgs-Derive"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_never_names_the_op_args_trait_directly() -> TestResult {
        let flat = expand_with_args_type(quote!(DemoArgs))?;

        assert!(
            !flat.contains("asOpArgsSchema>::json_schema"),
            "eine unbedingte Trait-Qualifizierung würde Args-Typen ohne \
             OpArgs-Derive brechen"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_probe_follows_a_path_qualified_args_type() -> TestResult {
        let flat = expand_with_args_type(quote!(crate::args::PlanArgs))?;

        assert!(
            flat.contains(
                "::harw_operations::operation::ArgsSchemaProbe::<crate::args::PlanArgs>::new()"
            ),
            "auch ein pfadqualifizierter Argument-Typ muss übernommen werden"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_rejects_non_async_fn() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer"
        };
        let args = parse_operation_args(attr)
            .map_err(ctx("die Attribut-Schlüssel sind gültig und müssen parsen"))?;
        let Err(error) = expand_operation(func, args) else {
            return Err(TestError::Unexpected(
                "eine nicht-async fn muss abgewiesen werden".to_owned(),
            ));
        };

        assert_eq!(error.to_string(), "#[operation] requires an `async fn`");
        Ok(())
    }

    /// Expandiert eine minimale Operation mit dem übergebenen rohen
    /// `#[operation(...)]`-Attribut-Tokenstrom (statt der festen
    /// `model_tool(readonly)`-Variante aus [`expand_with_args_type`]).
    fn expand_with_attr(attr: proc_macro2::TokenStream) -> TestResult<String> {
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let args = parse_operation_args(attr)
            .map_err(ctx("die Attribut-Schlüssel sind gültig und müssen parsen"))?;
        let tokens =
            expand_operation(func, args).map_err(ctx("eine gültige Operation muss expandieren"))?;
        Ok(normalize(&tokens))
    }

    #[test]
    fn channel_reduced_requires_explicit_remote_subcommands() -> TestResult {
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "channel_reduced")
        };
        let args = parse_operation_args(attr)
            .map_err(ctx("channel_reduced muss syntaktisch parsbar bleiben"))?;
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let Err(error) = expand_operation(func, args) else {
            return Err(TestError::Unexpected(
                "channel_reduced ohne channel_subcommands muss fehlschlagen".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains(r#"requires `channel_subcommands = "..."`"#)
        );
        Ok(())
    }

    #[test]
    fn channel_reduced_emits_remote_subcommand_allowlist() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(
                path = "/demo",
                visibility = "channel_reduced",
                channel_subcommands = "-,show,list"
            )
        })?;

        assert!(flat.contains("fnchannel_subcommands(&self)->&'static[&'staticstr]"));
        assert!(flat.contains("constTABLE:&[&str]=&[\"-\",\"show\",\"list\"]"));
        Ok(())
    }

    #[test]
    fn expand_operation_web_with_path_only_fails_missing_method() -> TestResult {
        // W3/C-OPS, F-031: `method` no longer defaults from `readonly` — a
        // `web(...)` without it must fail to expand with a clear message.
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(path = "/api/demo")
        };
        let args = parse_operation_args(attr).map_err(ctx(
            "`web(path = ...)` ohne `method` muss beim Parsen noch durchgehen",
        ))?;
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let Err(error) = expand_operation(func, args) else {
            return Err(TestError::Unexpected(
                "`web(...)` ohne `method` muss beim Expandieren fehlschlagen".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "`web(...)` requires a `method = \"get\"` or `method = \"post\"` key"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_web_method_and_approval_are_honored() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(path = "/api/demo", method = "post", approval = "always")
        })?;

        assert!(flat.contains("method:::harw_operations::operation::WebMethod::Post"));
        assert!(flat.contains("approval:::harw_operations::ApprovalPolicy::Always"));
        Ok(())
    }

    #[test]
    fn expand_operation_web_method_get_is_honored() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(path = "/api/demo", method = "get")
        })?;

        assert!(flat.contains("method:::harw_operations::operation::WebMethod::Get"));
        assert!(flat.contains("approval:::harw_operations::ApprovalPolicy::None"));
        Ok(())
    }

    #[test]
    fn expand_operation_web_rejects_unknown_method_value() -> TestResult {
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(path = "/api/demo", method = "put")
        };
        let args = parse_operation_args(attr)
            .map_err(ctx("`method = \"put\"` muss beim Parsen noch durchgehen"))?;
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let Err(error) = expand_operation(func, args) else {
            return Err(TestError::Unexpected(
                "ein unbekannter `method`-Wert muss beim Expandieren fehlschlagen".to_owned(),
            ));
        };
        assert!(
            error.to_string().contains("unknown `method` value `put`"),
            "unerwartete Fehlermeldung: {error}"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_web_coexists_with_command_and_model_tool() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "tui_only"),
            model_tool(readonly, approval = "none"),
            web(path = "/api/demo", method = "get", approval = "none")
        })?;

        // Alle drei Flächen müssen nebeneinander im `surfaces`-Vec landen — keine
        // ersetzt eine andere.
        assert!(flat.contains("::harw_operations::Surface::Command{path:\"/demo\""));
        assert!(flat.contains("::harw_operations::Surface::ModelTool{readonly:true"));
        assert!(flat.contains("::harw_operations::Surface::Web{path:\"/api/demo\""));
        assert!(flat.contains("method:::harw_operations::operation::WebMethod::Get"));
        Ok(())
    }

    #[test]
    fn parse_operation_args_web_requires_path_at_expand_time() -> TestResult {
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(method = "get")
        };
        let args = parse_operation_args(attr).map_err(ctx(
            "`web(method = ...)` ohne `path` muss beim Parsen noch durchgehen",
        ))?;
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let Err(error) = expand_operation(func, args) else {
            return Err(TestError::Unexpected(
                "`web(...)` ohne `path` muss beim Expandieren fehlschlagen".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "`web(...)` requires a `path = \"...\"` key"
        );
        Ok(())
    }

    #[test]
    fn parse_operation_args_web_requires_method_at_expand_time() -> TestResult {
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(path = "/api/demo")
        };
        let args = parse_operation_args(attr).map_err(ctx(
            "`web(path = ...)` ohne `method` muss beim Parsen noch durchgehen",
        ))?;
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let Err(error) = expand_operation(func, args) else {
            return Err(TestError::Unexpected(
                "`web(...)` ohne `method` muss beim Expandieren fehlschlagen".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "`web(...)` requires a `method = \"get\"` or `method = \"post\"` key"
        );
        Ok(())
    }

    #[test]
    fn parse_operation_args_rejects_unknown_web_key() -> TestResult {
        let Err(error) = parse_operation_args(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(bogus = "x")
        }) else {
            return Err(TestError::Unexpected(
                "ein unbekannter `web`-Schlüssel muss abgewiesen werden".to_owned(),
            ));
        };

        assert!(
            error
                .to_string()
                .contains("unsupported `operation` `web` key")
        );
        Ok(())
    }

    #[test]
    fn parse_operation_args_web_rejects_readonly_key() -> TestResult {
        // W3/C-OPS, F-031: `readonly` was removed from `web(...)`'s grammar —
        // the HTTP method is no longer derivable from a readonly flag.
        let Err(error) = parse_operation_args(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            web(path = "/api/demo", readonly)
        }) else {
            return Err(TestError::Unexpected(
                "`web(readonly)` muss seit F-031 abgewiesen werden".to_owned(),
            ));
        };

        assert!(
            error
                .to_string()
                .contains("unsupported `operation` `web` key"),
            "unerwartete Fehlermeldung: {error}"
        );
        Ok(())
    }

    // ── `command(...)` `busy` sub-key ────────────────────────────────────────

    #[test]
    fn expand_operation_command_busy_immediate_is_honored() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "tui_only", busy = "immediate")
        })?;

        assert!(
            flat.contains("busy:::harw_operations::operation::BusyAvailability::Immediate"),
            "`busy = \"immediate\"` muss auf BusyAvailability::Immediate abgebildet werden"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_command_busy_deferred_is_honored() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "tui_only", busy = "deferred")
        })?;

        assert!(
            flat.contains(
                "busy:::harw_operations::operation::BusyAvailability::DeferredUntilTurnEnd"
            ),
            "`busy = \"deferred\"` muss auf BusyAvailability::DeferredUntilTurnEnd abgebildet werden"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_without_busy_key_defaults_to_deferred() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "tui_only")
        })?;

        assert!(
            flat.contains(
                "busy:::harw_operations::operation::BusyAvailability::DeferredUntilTurnEnd"
            ),
            "ein fehlender `busy`-Schlüssel muss auf den Default DeferredUntilTurnEnd fallen"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_rejects_unknown_busy_value() -> TestResult {
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "tui_only", busy = "invalid-wert")
        };
        let args = parse_operation_args(attr).map_err(ctx(
            "`busy = \"invalid-wert\"` muss beim Parsen noch durchgehen",
        ))?;
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let Err(error) = expand_operation(func, args) else {
            return Err(TestError::Unexpected(
                "ein unbekannter `busy`-Wert muss beim Expandieren fehlschlagen".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "busy muss 'immediate', 'staged' oder 'deferred' sein, war: 'invalid-wert'"
        );
        Ok(())
    }

    #[test]
    fn expand_operation_command_busy_staged_is_honored() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "tui_only", busy = "staged")
        })?;
        assert!(flat.contains("busy:::harw_operations::operation::BusyAvailability::Staged"));
        Ok(())
    }

    #[test]
    fn expand_operation_busy_subcommands_emits_override() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(
                path = "/demo",
                visibility = "tui_only",
                busy = "staged",
                busy_subcommands = "show=immediate, -=immediate, test=deferred"
            )
        })?;
        assert!(flat.contains("fnbusy_subcommands(&self,)"), "{flat}");
        assert!(flat.contains(
            "BusySubcommand::new(::core::option::Option::Some(\"show\"),::harw_operations::operation::BusyAvailability::Immediate)"
        ));
        assert!(flat.contains(
            "BusySubcommand::new(::core::option::Option::None,::harw_operations::operation::BusyAvailability::Immediate)"
        ));
        assert!(flat.contains(
            "BusySubcommand::new(::core::option::Option::Some(\"test\"),::harw_operations::operation::BusyAvailability::DeferredUntilTurnEnd)"
        ));
        Ok(())
    }

    #[test]
    fn expand_operation_without_busy_subcommands_has_no_override() -> TestResult {
        let flat = expand_with_attr(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", visibility = "tui_only", busy = "immediate")
        })?;
        assert!(!flat.contains("fnbusy_subcommands"));
        Ok(())
    }

    #[test]
    fn expand_operation_rejects_malformed_busy_subcommands() -> TestResult {
        for (value, needle) in [
            ("show", "braucht die Form"),
            ("show=sofort", "Klasse muss"),
            ("show=immediate, show=staged", "doppelt"),
            (" , ", "mindestens ein Eintrag"),
        ] {
            let attr = quote! {
                name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
                command(path = "/demo", visibility = "tui_only", busy_subcommands = #value)
            };
            let args = parse_operation_args(attr).map_err(ctx("parse"))?;
            let func: ItemFn = syn::parse_quote! {
                async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                    let _ = (ctx, args);
                    Ok(OpOutput { text: String::new() })
                }
            };
            let Err(error) = expand_operation(func, args) else {
                return Err(TestError::Unexpected(format!(
                    "'{value}' muss abgewiesen werden"
                )));
            };
            assert!(error.to_string().contains(needle), "{value}: {error}");
        }
        Ok(())
    }

    #[test]
    fn parse_operation_args_rejects_unknown_command_key() -> TestResult {
        let Err(error) = parse_operation_args(quote! {
            name = "demo", summary = "Demo.", domain = "misc", permission = "observer",
            command(path = "/demo", bogus = "x")
        }) else {
            return Err(TestError::Unexpected(
                "ein unbekannter `command`-Schlüssel muss abgewiesen werden".to_owned(),
            ));
        };

        assert!(
            error
                .to_string()
                .contains("unsupported `operation` `command` key"),
            "unerwartete Fehlermeldung: {error}"
        );
        Ok(())
    }

    #[test]
    fn parse_operation_args_rejects_unknown_top_level_key() -> TestResult {
        let Err(error) = parse_operation_args(quote!(unknown = "x")) else {
            return Err(TestError::Unexpected(
                "ein unbekannter Schlüssel muss abgewiesen werden".to_owned(),
            ));
        };

        assert!(
            error
                .to_string()
                .contains("unsupported `operation` attribute key")
        );
        Ok(())
    }

    /// Expandiert eine minimale Operation mit dem übergebenen `domain`-Literal.
    fn expand_with_domain(domain: &str) -> TestResult<String> {
        let func: ItemFn = syn::parse_quote! {
            async fn demo_op(ctx: &OpContext, args: DemoArgs) -> Result<OpOutput, OpError> {
                let _ = (ctx, args);
                Ok(OpOutput { text: String::new() })
            }
        };
        let attr = quote! {
            name = "demo", summary = "Demo.", domain = #domain,
            permission = "maintainer", model_tool(readonly)
        };
        let args = parse_operation_args(attr)
            .map_err(ctx("die Attribut-Schlüssel sind gültig und müssen parsen"))?;
        let tokens =
            expand_operation(func, args).map_err(ctx("eine gültige Operation muss expandieren"))?;
        Ok(normalize(&tokens))
    }

    #[test]
    fn map_domain_accepts_infrastructure_domains() -> TestResult {
        for (slug, variant) in [
            ("identity", "Identity"),
            ("network", "Network"),
            ("security", "Security"),
            ("crypto", "Crypto"),
        ] {
            let lit = syn::LitStr::new(slug, proc_macro2::Span::call_site());
            let tokens = map_domain(&lit).map_err(ctx("Infrastruktur-Domäne muss mappen"))?;
            assert_eq!(
                normalize(&tokens),
                format!("::harw_operations::OperationDomain::{variant}")
            );
        }
        Ok(())
    }

    #[test]
    fn map_domain_rejects_unknown_and_lists_all_domains() -> TestResult {
        let lit = syn::LitStr::new("infra", proc_macro2::Span::call_site());
        let Err(error) = map_domain(&lit) else {
            return Err(TestError::Unexpected(
                "eine unbekannte Domäne muss abgewiesen werden".to_owned(),
            ));
        };
        let message = error.to_string();
        for slug in [
            "session",
            "agents",
            "execution",
            "catalog_config",
            "knowledge",
            "identity",
            "network",
            "security",
            "crypto",
            "misc",
        ] {
            assert!(message.contains(slug), "`{slug}` fehlt in: {message}");
        }
        Ok(())
    }

    #[test]
    fn expand_operation_maps_infrastructure_domains_and_system_category() -> TestResult {
        for (slug, variant) in [
            ("identity", "Identity"),
            ("network", "Network"),
            ("security", "Security"),
            ("crypto", "Crypto"),
        ] {
            let flat = expand_with_domain(slug)?;
            assert!(
                flat.contains(&format!(
                    "domain:::harw_operations::OperationDomain::{variant}"
                )),
                "Domäne `{slug}` muss auf `{variant}` expandieren: {flat}"
            );
            assert!(
                flat.contains("category:::harw_operations::OperationCategory::System"),
                "Domäne `{slug}` muss die Kategorie `System` ableiten"
            );
        }
        Ok(())
    }
}
