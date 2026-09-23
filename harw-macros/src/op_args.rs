//! `#[derive(OpArgs)]`-Expansion — JSON-Schema für Operations-Argumente.
//!
//! Erzeugt aus einem Argument-Typ eine Implementierung von
//! `::harw_operations::OpArgsSchema`, damit die Modell-Tool-Fläche ein
//! **geschlossenes** Schema (`additionalProperties: false`) mit `properties`
//! und `required` veröffentlichen kann, statt eines offenen Objekt-Fallbacks.
//!
//! # Verantwortungsbereich
//! - Struct-Argumente: ein Feld ⇒ eine Property.
//! - Subcommand-Enums (`#[raw(subcommand)]`): eine Pflicht-Property `action`
//!   (`type: string`, `enum: [<kebab-case-Varianten>]`) plus die **vereinigten**
//!   Properties aller Varianten-Felder (alle optional, weil sie nur je Variante
//!   gelten) plus eine Wurzel-`description`, die alle Subcommands mit ihren
//!   Feldern auflistet.
//!
//! Die Typ-Inferenz selbst wird **nicht** dupliziert, sondern aus
//! [`crate::schema`] wiederverwendet (`schema_for_type`, `scalar_json_type`,
//! `option_inner`, `vec_inner`, `doc_string`, `field_default`). Die
//! Schema-Bauhelfer leben in `harw_operations::op_schema`.
//!
//! `expand_op_args` ist der Einstiegspunkt, den die
//! `#[proc_macro_derive(OpArgs, ...)]`-Funktion im Crate-Root (`lib.rs`)
//! aufruft.

use crate::schema::{
    doc_string, field_default, option_inner, scalar_json_type, schema_for_type, vec_inner,
};
use quote::{ToTokens, quote};
use syn::{Data, DataEnum, DataStruct, DeriveInput, Fields, Type};

/// Property-Name, den ein Subcommand-Enum für die Variantenwahl belegt.
const ACTION_PROPERTY: &str = "action";

/// Expander für das `OpArgs`-Derive-Makro.
///
/// # Beschreibung
/// Verzweigt nach Datenform: benannte Structs erzeugen ein flaches
/// Property-Objekt, Enums ein Subcommand-Objekt mit `action`-Property.
/// Alle Diagnosen laufen vor der Code-Erzeugung.
///
/// # Argumente
/// - `input` (`&DeriveInput`): geparster Argument-Typ.
///
/// # Rückgabe
/// Token-Strom mit `impl ::harw_operations::OpArgsSchema for <Typ>`.
///
/// # Errors
/// - Tupel-Struct oder Unit-Struct.
/// - `union`-Typ.
/// - Enum ohne Varianten.
/// - Enum-Variante mit Tupelfeldern.
/// - Enum-Varianten mit demselben kebab-case-Namen.
/// - Enum-Variantenfeld mit dem reservierten Namen `action`.
/// - Gleichnamige Variantenfelder mit unterschiedlichem (entwrapptem) Typ.
/// - Unbekannter Schlüssel in `#[raw(...)]` auf Enum-Ebene.
/// - Nicht unterstützter Feldtyp (aus [`schema_for_type`]).
pub(crate) fn expand_op_args(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let body = match &input.data {
        Data::Struct(data) => expand_struct(input, data)?,
        Data::Enum(data) => expand_enum(input, data)?,
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                input,
                "derive(OpArgs) unterstützt keine `union`-Typen; erwartet: Struct mit benannten Feldern oder Subcommand-Enum",
            ));
        }
    };

    Ok(quote! {
        impl #impl_generics ::harw_operations::OpArgsSchema for #ident #ty_generics #where_clause {
            fn json_schema() -> ::harw_tools::JsonSchema {
                #body
            }
        }
    })
}

// ── Struct-Pfad ───────────────────────────────────────────────────────────────

/// Baut das Objekt-Schema eines Argument-Structs: ein Feld ⇒ eine Property.
///
/// `Option<T>`-Felder und Felder mit `#[tool(default = ...)]` sind nicht
/// `required`; alle übrigen schon — dieselbe Regel wie in `#[derive(Tool)]`.
fn expand_struct(input: &DeriveInput, data: &DataStruct) -> syn::Result<proc_macro2::TokenStream> {
    let fields = match &data.fields {
        Fields::Named(named) => &named.named,
        Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                input,
                "derive(OpArgs) erfordert benannte Felder; Tupel-Structs haben keine Property-Namen",
            ));
        }
        Fields::Unit => {
            return Err(syn::Error::new_spanned(
                input,
                "derive(OpArgs) erfordert benannte Felder; ein Unit-Struct deklariert keine Argumente",
            ));
        }
    };

    let mut properties: Vec<proc_macro2::TokenStream> = Vec::with_capacity(fields.len());
    let mut required: Vec<String> = Vec::new();

    for field in fields {
        let name = field
            .ident
            .as_ref()
            .ok_or_else(|| {
                syn::Error::new_spanned(
                    field,
                    "derive(OpArgs) erfordert einen Bezeichner für jedes Feld",
                )
            })?
            .to_string();
        let default = field_default(field)?;
        let description = doc_string(&field.attrs);
        let (schema_expr, optional) =
            schema_for_type(&field.ty, description.as_deref(), default.as_ref())?;

        properties.push(quote! { (#name, #schema_expr) });
        if !optional && default.is_none() {
            required.push(name);
        }
    }

    Ok(quote! {
        ::harw_operations::op_schema::object_schema(
            ::std::vec![ #(#properties),* ],
            &[ #(#required),* ],
        )
    })
}

// ── Enum-Pfad (Subcommand) ────────────────────────────────────────────────────

/// Eine bereits aufgenommene Union-Property eines Subcommand-Enums.
struct UnionProperty {
    /// Property-Name (Feldname der ersten Variante, die ihn deklariert).
    name: String,
    /// Entwrappter Feldtyp als Token-Text — Grundlage der Konfliktprüfung.
    unwrapped_type: String,
}

/// Baut das Subcommand-Objekt-Schema eines Argument-Enums.
///
/// Erzeugt die Pflicht-Property `action` (String-Enum aller kebab-case-Varianten),
/// vereinigt alle Varianten-Felder als optionale Properties und schreibt eine
/// Wurzel-`description`, die jeden Subcommand mit seinen Feldern auflistet.
fn expand_enum(input: &DeriveInput, data: &DataEnum) -> syn::Result<proc_macro2::TokenStream> {
    validate_subcommand_marker(&input.attrs)?;

    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "derive(OpArgs) erfordert mindestens eine Enum-Variante; ein leeres Enum hat keinen Subcommand",
        ));
    }

    let mut actions: Vec<String> = Vec::with_capacity(data.variants.len());
    let mut properties: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut seen: Vec<UnionProperty> = Vec::new();
    let mut description_lines: Vec<String> = Vec::with_capacity(data.variants.len());

    for variant in &data.variants {
        let action = kebab_case(&variant.ident.to_string());
        if actions.contains(&action) {
            return Err(syn::Error::new_spanned(
                &variant.ident,
                format!(
                    "derive(OpArgs): zwei Varianten ergeben denselben Subcommand-Namen `{action}`"
                ),
            ));
        }

        let fields = match &variant.fields {
            Fields::Named(named) => named.named.iter().collect::<Vec<_>>(),
            Fields::Unit => Vec::new(),
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    &variant.fields,
                    format!(
                        "derive(OpArgs): Variante `{}` hat Tupelfelder; Subcommand-Varianten brauchen benannte Felder (oder gar keine)",
                        variant.ident
                    ),
                ));
            }
        };

        let mut field_summaries: Vec<String> = Vec::with_capacity(fields.len());

        for field in fields {
            let field_ident = field.ident.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    field,
                    "derive(OpArgs) erfordert einen Bezeichner für jedes Feld",
                )
            })?;
            let name = field_ident.to_string();

            if name == ACTION_PROPERTY {
                return Err(syn::Error::new_spanned(
                    field_ident,
                    format!(
                        "derive(OpArgs): der Feldname `{ACTION_PROPERTY}` ist für die Subcommand-Auswahl reserviert"
                    ),
                ));
            }

            let default = field_default(field)?;
            let description = doc_string(&field.attrs);
            let (schema_expr, optional) =
                schema_for_type(&field.ty, description.as_deref(), default.as_ref())?;

            let unwrapped_type = unwrapped_type_text(&field.ty);
            let type_name = json_type_name(&field.ty).unwrap_or_else(|| "wert".to_owned());
            let requiredness = if optional || default.is_some() {
                "optional"
            } else {
                "erforderlich"
            };
            field_summaries.push(format!("`{name}` ({type_name}, {requiredness})"));

            // `position` statt `find`, damit kein Borrow auf `seen` in den
            // Push-Zweig hineinreicht.
            match seen.iter().position(|property| property.name == name) {
                Some(index) => {
                    // Gleicher Typ: die erste Variante bestimmt Schema und Beschreibung.
                    if seen[index].unwrapped_type != unwrapped_type {
                        return Err(syn::Error::new_spanned(
                            &field.ty,
                            format!(
                                "derive(OpArgs): Feld `{name}` hat in mehreren Varianten unterschiedliche Typen (`{}` vs. `{unwrapped_type}`); die vereinigte Property kann nur einen Typ haben",
                                seen[index].unwrapped_type
                            ),
                        ));
                    }
                }
                None => {
                    properties.push(quote! { (#name, #schema_expr) });
                    seen.push(UnionProperty {
                        name,
                        unwrapped_type,
                    });
                }
            }
        }

        description_lines.push(variant_summary_line(
            &action,
            doc_string(&variant.attrs).as_deref(),
            &field_summaries,
        ));
        actions.push(action);
    }

    let root_description = root_description(&description_lines);
    let action_description = format!(
        "Auszuführender Subcommand. Erlaubt: {}.",
        actions
            .iter()
            .map(|action| format!("`{action}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    Ok(quote! {
        ::harw_operations::op_schema::described_object_schema(
            #root_description,
            ::std::vec![
                (
                    #ACTION_PROPERTY,
                    ::harw_operations::op_schema::enum_string_schema(
                        #action_description,
                        &[ #(#actions),* ],
                    )
                ),
                #(#properties),*
            ],
            &[ #ACTION_PROPERTY ],
        )
    })
}

/// Prüft ein optionales `#[raw(...)]` auf Enum-Ebene: einziger erlaubter
/// Schlüssel ist `subcommand`. Fehlt das Attribut ganz, ist das zulässig — das
/// Enum wird dann trotzdem als Subcommand-Enum behandelt.
fn validate_subcommand_marker(attrs: &[syn::Attribute]) -> syn::Result<()> {
    for attr in attrs {
        if !attr.path().is_ident("raw") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("subcommand") {
                Ok(())
            } else {
                Err(meta
                    .error("unbekannter `raw`-Schlüssel auf Enum-Ebene; erwartet: `subcommand`"))
            }
        })?;
    }
    Ok(())
}

// ── Beschreibungstexte ────────────────────────────────────────────────────────

/// Baut die Aufzählungszeile eines Subcommands für die Wurzel-`description`.
fn variant_summary_line(action: &str, doc: Option<&str>, field_summaries: &[String]) -> String {
    let mut line = format!("- `{action}`");
    if let Some(doc) = doc.map(str::trim).filter(|doc| !doc.is_empty()) {
        line.push_str(" — ");
        line.push_str(doc);
    }
    if field_summaries.is_empty() {
        line.push_str(" Keine Felder.");
    } else {
        line.push_str(" Felder: ");
        line.push_str(&field_summaries.join(", "));
        line.push('.');
    }
    line
}

/// Setzt Kopfzeile und Subcommand-Zeilen zur Wurzel-`description` zusammen.
fn root_description(lines: &[String]) -> String {
    let mut text = String::from(
        "Subcommand-Aufruf: `action` wählt den Subcommand; die übrigen Properties gelten jeweils nur für den gewählten Subcommand.",
    );
    for line in lines {
        text.push('\n');
        text.push_str(line);
    }
    text
}

// ── Typ-Hilfen ────────────────────────────────────────────────────────────────

/// Benennt den JSON-Typ eines Feldes für die Fließtext-Beschreibung.
///
/// Leitet den Namen aus [`scalar_json_type`] ab, damit die Zuordnung
/// Rust-Typ → JSON-Typ **nicht** dupliziert wird. `Option<T>` wird entwrappt,
/// `Vec<T>` als `array<T>` beschrieben.
fn json_type_name(ty: &Type) -> Option<String> {
    let inner = option_inner(ty).unwrap_or(ty);
    if let Some(item) = vec_inner(inner) {
        let item_name = json_type_name(item).unwrap_or_else(|| "wert".to_owned());
        return Some(format!("array<{item_name}>"));
    }
    let path = scalar_json_type(inner)?.to_string();
    let last = path.rsplit("::").next()?.trim().to_ascii_lowercase();
    if last.is_empty() { None } else { Some(last) }
}

/// Token-Text des entwrappten Feldtyps (`Option<T>` ⇒ `T`), normalisiert auf
/// einfache Leerzeichen — Vergleichsgrundlage für die Konfliktprüfung
/// gleichnamiger Variantenfelder.
fn unwrapped_type_text(ty: &Type) -> String {
    let inner = option_inner(ty).unwrap_or(ty);
    inner
        .to_token_stream()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wandelt einen `PascalCase`-Variantennamen in `kebab-case`.
///
/// Behandelt Akronym-Läufe (`HTTPGet` ⇒ `http-get`) und bereits vorhandene
/// Trennzeichen (`Research_Deps` ⇒ `research-deps`).
fn kebab_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);

    for (idx, ch) in chars.iter().enumerate() {
        if *ch == '_' || *ch == '-' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            continue;
        }
        if ch.is_uppercase() && !out.is_empty() && !out.ends_with('-') {
            let previous = chars[idx - 1];
            let next_is_lower = chars.get(idx + 1).copied().is_some_and(char::is_lowercase);
            if !previous.is_uppercase() || next_is_lower {
                out.push('-');
            }
        }
        out.extend(ch.to_lowercase());
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{expand_op_args, json_type_name, kebab_case, variant_summary_line};
    use crate::test_support::{TestError, TestResult, ctx};
    use syn::DeriveInput;

    fn expand(input: DeriveInput) -> TestResult<String> {
        Ok(expand_op_args(&input)
            .map_err(ctx("unterstützter Argument-Typ muss expandieren"))?
            .to_string())
    }

    // ── Struct-Pfad ───────────────────────────────────────────────────────────

    #[test]
    fn test_expand_op_args_struct_emits_op_args_schema_impl() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            struct StopArgs {
                job_id: String,
            }
        })?;

        assert!(
            expanded.contains("impl :: harw_operations :: OpArgsSchema for StopArgs"),
            "erzeugt wurde: {expanded}"
        );
        assert!(expanded.contains("fn json_schema () -> :: harw_tools :: JsonSchema"));
        assert!(expanded.contains(":: harw_operations :: op_schema :: object_schema"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_struct_field_becomes_property() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            struct DiffArgs {
                path: String,
                stat_only: bool,
            }
        })?;

        assert!(
            expanded.contains("(\"path\" ,"),
            "erzeugt wurde: {expanded}"
        );
        assert!(expanded.contains("(\"stat_only\" ,"));
        assert!(expanded.contains(":: harw_tools :: JsonSchemaType :: String"));
        assert!(expanded.contains(":: harw_tools :: JsonSchemaType :: Boolean"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_struct_non_option_field_is_required() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            struct StopArgs {
                job_id: String,
            }
        })?;

        assert!(
            expanded.contains("& [\"job_id\"]"),
            "erzeugt wurde: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_expand_op_args_struct_option_field_is_not_required() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            struct PsArgs {
                status: Option<String>,
            }
        })?;

        assert!(
            expanded.contains("(\"status\" ,"),
            "erzeugt wurde: {expanded}"
        );
        assert!(
            expanded.contains("& []"),
            "Option-Felder dürfen nicht in `required` landen: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_expand_op_args_struct_default_field_is_not_required() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            struct SearchArgs {
                #[tool(default = 25)]
                page_size: u8,
            }
        })?;

        assert!(
            expanded.contains("& []"),
            "Felder mit Default sind nicht erforderlich: {expanded}"
        );
        assert!(expanded.contains(":: harw_tools :: serde_json :: json ! (25)"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_struct_doc_comment_becomes_description() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            struct DiffArgs {
                /// Pfad relativ zum Workspace.
                path: Option<String>,
            }
        })?;

        assert!(
            expanded.contains("\"Pfad relativ zum Workspace.\""),
            "erzeugt wurde: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_expand_op_args_struct_vec_field_becomes_array() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            struct ExploreArgs {
                paths: Vec<String>,
            }
        })?;

        assert!(
            expanded.contains(":: harw_tools :: JsonSchemaType :: Array"),
            "erzeugt wurde: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_expand_op_args_tuple_struct_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct StopArgs(String);
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected(
                "Tupel-Structs müssen abgewiesen werden".to_owned(),
            ));
        };
        assert!(error.to_string().contains("benannte Felder"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_unit_struct_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct StatusArgs;
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected(
                "Unit-Structs müssen abgewiesen werden".to_owned(),
            ));
        };
        assert!(error.to_string().contains("benannte Felder"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_unsupported_field_type_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct WeirdArgs {
                handle: std::net::TcpStream,
            }
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected(
                "nicht unterstützte Feldtypen müssen scheitern".to_owned(),
            ));
        };
        assert!(error.to_string().contains("unsupported tool field type"));
        Ok(())
    }

    // ── Enum-Pfad ─────────────────────────────────────────────────────────────

    #[test]
    fn test_expand_op_args_enum_emits_action_enum_list() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show,
                ResearchDeps,
            }
        })?;

        assert!(
            expanded.contains(":: harw_operations :: op_schema :: enum_string_schema"),
            "erzeugt wurde: {expanded}"
        );
        assert!(expanded.contains("& [\"show\" , \"research-deps\"]"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_action_is_the_only_required_property() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show { id: String },
            }
        })?;

        assert!(
            expanded.contains("& [\"action\"]"),
            "nur `action` darf erforderlich sein: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_unions_variant_fields_as_properties() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show { id: String },
                Add { title: String, depth: Option<u8> },
            }
        })?;

        assert!(expanded.contains("(\"id\" ,"), "erzeugt wurde: {expanded}");
        assert!(expanded.contains("(\"title\" ,"));
        assert!(expanded.contains("(\"depth\" ,"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_uses_described_object_schema() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                /// Zeigt den Plan.
                Show { id: String },
            }
        })?;

        assert!(
            expanded.contains(":: harw_operations :: op_schema :: described_object_schema"),
            "erzeugt wurde: {expanded}"
        );
        assert!(expanded.contains("Zeigt den Plan."));
        assert!(expanded.contains("`id` (string, erforderlich)"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_duplicate_field_keeps_first_description() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show {
                    /// Erste Beschreibung.
                    id: String,
                },
                Drop {
                    /// Zweite Beschreibung.
                    id: String,
                },
            }
        })?;

        assert!(
            expanded.contains("Erste Beschreibung."),
            "erzeugt wurde: {expanded}"
        );
        assert!(
            !expanded.contains("Zweite Beschreibung."),
            "die erste Variante bestimmt die Property-Beschreibung: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_optional_and_required_same_field_is_accepted() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show { id: String },
                List { id: Option<String> },
            }
        })?;

        assert!(expanded.contains("(\"id\" ,"), "erzeugt wurde: {expanded}");
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_conflicting_field_types_are_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show { id: String },
                Drop { id: u32 },
            }
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected(
                "Typkonflikte müssen scheitern".to_owned(),
            ));
        };
        assert!(error.to_string().contains("unterschiedliche Typen"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_tuple_variant_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show(String),
            }
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected(
                "Tupel-Varianten müssen scheitern".to_owned(),
            ));
        };
        assert!(error.to_string().contains("Tupelfelder"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_reserved_action_field_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {
                Show { action: String },
            }
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected("`action` ist reserviert".to_owned()));
        };
        assert!(error.to_string().contains("reserviert"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_without_variants_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum PlanArgs {}
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected(
                "leere Enums müssen scheitern".to_owned(),
            ));
        };
        assert!(error.to_string().contains("mindestens eine Enum-Variante"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_unknown_raw_key_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(positional)]
            enum PlanArgs {
                Show,
            }
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected(
                "unbekannte raw-Schlüssel müssen scheitern".to_owned(),
            ));
        };
        assert!(error.to_string().contains("subcommand"));
        Ok(())
    }

    #[test]
    fn test_expand_op_args_enum_without_raw_marker_still_expands() -> TestResult {
        let expanded = expand(syn::parse_quote! {
            enum PlanArgs {
                Show,
            }
        })?;

        assert!(
            expanded.contains("& [\"action\"]"),
            "erzeugt wurde: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_expand_op_args_union_is_rejected() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            union Weird {
                a: u32,
            }
        };

        let Err(error) = expand_op_args(&input) else {
            return Err(TestError::Unexpected("unions müssen scheitern".to_owned()));
        };
        assert!(error.to_string().contains("union"));
        Ok(())
    }

    // ── Reine Hilfsfunktionen ─────────────────────────────────────────────────

    #[test]
    fn test_kebab_case_splits_pascal_case() {
        assert_eq!(kebab_case("ResearchDeps"), "research-deps");
        assert_eq!(kebab_case("Web"), "web");
        assert_eq!(kebab_case("ResearchWeb"), "research-web");
    }

    #[test]
    fn test_kebab_case_handles_acronym_runs() {
        assert_eq!(kebab_case("HTTPGet"), "http-get");
        assert_eq!(kebab_case("ID"), "id");
    }

    #[test]
    fn test_kebab_case_treats_underscore_as_separator() {
        assert_eq!(kebab_case("Research_Deps"), "research-deps");
        assert_eq!(kebab_case("plan_show"), "plan-show");
    }

    #[test]
    fn test_json_type_name_maps_scalars_and_containers() {
        assert_eq!(
            json_type_name(&syn::parse_quote!(String)).as_deref(),
            Some("string")
        );
        assert_eq!(
            json_type_name(&syn::parse_quote!(Option<u8>)).as_deref(),
            Some("integer")
        );
        assert_eq!(
            json_type_name(&syn::parse_quote!(bool)).as_deref(),
            Some("boolean")
        );
        assert_eq!(
            json_type_name(&syn::parse_quote!(Vec<String>)).as_deref(),
            Some("array<string>")
        );
    }

    #[test]
    fn test_variant_summary_line_without_fields() {
        let line = variant_summary_line("show", Some("Zeigt den Plan."), &[]);

        assert_eq!(line, "- `show` — Zeigt den Plan. Keine Felder.");
    }

    #[test]
    fn test_variant_summary_line_with_fields_and_without_doc() {
        let line =
            variant_summary_line("add", None, &["`title` (string, erforderlich)".to_owned()]);

        assert_eq!(line, "- `add` Felder: `title` (string, erforderlich).");
    }
}
