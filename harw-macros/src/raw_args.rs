//! `#[derive(FromRawArgs)]`-Expansion für Argument-Structs **und**
//! Subcommand-Enums.
//!
//! # Verantwortungsbereich
//!
//! Dieses Modul besitzt die gesamte Compile-Zeit-Analyse und Code-Erzeugung für
//! `#[derive(FromRawArgs)]`. Es implementiert
//! `harw_operations::FromRawArgs::from_raw_args(tokens: &[String]) -> Result<Self, OpError>`
//! in zwei Ausprägungen:
//!
//! - **Struct-Pfad** — jedes benannte Feld trägt genau ein `#[raw(...)]`-Attribut
//!   und greift direkt auf den vollen Token-Slice zu.
//! - **Enum-Pfad** — das Enum trägt `#[raw(subcommand)]`; das **erste** Token
//!   wählt die Variante, die **restlichen** Tokens werden an die Variantenfelder
//!   verteilt.
//!
//! Die eigentliche Laufzeit-Arbeit erledigen die Helfer aus
//! `harw_operations::args` (`first_optional`, `join_all_optional`, `join_from`,
//! `nth_optional`, `require_first`); dieses Modul erzeugt lediglich die Aufrufe.
//!
//! # ⚠ Semantik-Unterschied bei `nth` — Struct vs. Enum
//!
//! Das ist der wichtigste Fallstrick des Makros:
//!
//! | Kontext | Bezugs-Slice | `nth = 0` | Bedeutung von `nth = N` |
//! |---|---|---|---|
//! | **Struct** | der volle `tokens`-Slice | **Compile-Fehler** (nutze `first`) | `tokens[N]`, N ≥ 1 |
//! | **Enum-Variante** | `tokens[1..]` (alles nach dem Subcommand) | **erlaubt und üblich** | `tokens[1 + N]` |
//!
//! Im Enum-Pfad ist `nth` also **0-basiert relativ zum Rest**: `#[raw(nth = 0)]`
//! meint das Token **direkt nach** dem Subcommand. Bei `plan create my-id …`
//! liefert `#[raw(nth = 0)]` folglich `"my-id"`, nicht `"create"`.
//!
//! # Feld-Attribute
//!
//! - `#[raw(first)]` — erstes Token des Bezugs-Slices (oder `None`).
//! - `#[raw(join)]` — alle Tokens des Bezugs-Slices, mit Leerzeichen verbunden.
//! - `#[raw(nth = N)]` — N-tes Token (siehe Semantik-Tabelle oben).
//! - `#[raw(join_from = N)]` — alle Tokens ab Index N, mit Leerzeichen verbunden;
//!   `None`, wenn ab dort keine Tokens mehr da sind. Immer 0-basiert auf dem
//!   jeweiligen Bezugs-Slice, sowohl bei Structs als auch bei Enums.
//! - `#[raw(required)]` — erstes Token des Bezugs-Slices; fehlt es, liefert
//!   `from_raw_args` `Err(OpError::InvalidArguments)`. Höchstens einmal pro
//!   Struct bzw. pro Variante.
//!
//! Alle Felder müssen den Typ `Option<String>` haben.
//!
//! # Enum-Attribute
//!
//! - `#[raw(subcommand)]` am Enum — verpflichtend, markiert das Enum als
//!   Subcommand-Dispatch.
//! - `#[raw(alias = "…")]` an einer Variante — zusätzlicher Erkennungs-Token,
//!   mehrfach erlaubt.
//! - `#[raw(default_subcommand)]` an **genau einer** Variante — greift, wenn
//!   überhaupt kein Token vorhanden ist.
//!
//! Unit-Varianten (ohne Felder) sind erlaubt; überzählige Tokens werden dort
//! bewusst ignoriert, damit `/plan ready --verbose` nicht scheitert.
//!
//! # Fehlerarten
//!
//! Compile-Zeit: [`syn::Error`] für jede Diagnose (falscher Feldtyp,
//! widersprüchliche Attribute, fehlendes Attribut, mehrfaches `required`,
//! unbekannter Schlüssel, `nth = 0` im Struct-Pfad, Tupel-Variante, doppelter
//! Subcommand-Token, mehrfaches `default_subcommand`, leeres Enum).
//!
//! Laufzeit (im erzeugten Code): ausschließlich
//! `harw_operations::OpError::InvalidArguments`.
//!
//! # Nebenläufigkeit
//!
//! Reine Compile-Zeit-Logik ohne globalen Zustand; der erzeugte Code ist
//! zustandslos und damit `Send + Sync`.

use quote::{ToTokens, quote};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{Data, DeriveInput, Field, Fields, Type, Variant};

// ── Konstante Meldungsbausteine ──────────────────────────────────────────────

/// Aufzählung der gültigen Feld-Schlüssel; erscheint in mehreren Diagnosen.
const RAW_FIELD_KEYS: &str = "first, join, join_from = N, nth = N, required";

/// Meldung für `#[raw(nth = 0)]` im Struct-Pfad.
///
/// Erklärt zusätzlich den Semantik-Unterschied zum Enum-Pfad, weil genau diese
/// Verwechslung der häufigste Grund für den Fehler ist.
const NTH_ZERO_MSG: &str = "#[raw(nth = 0)] ist im Struct-Pfad nicht erlaubt; benutze #[raw(first)]. \
     Achtung — im Enum-Pfad (#[raw(subcommand)]) ist `nth` dagegen 0-basiert relativ zu den Tokens \
     nach dem Subcommand: dort ist `nth = 0` gültig und meint das Token direkt nach dem Subcommand";

/// Meldung für Tupel- und Unit-Structs.
const STRUCT_SHAPE_MSG: &str = "FromRawArgs verlangt ein Struct mit benannten Feldern";

// ── Typprüfung ───────────────────────────────────────────────────────────────

/// Prüft, ob `ty` exakt `Option<String>` ist (Pfadform, beliebig qualifiziert).
///
/// # Description
/// Akzeptiert sowohl `Option<String>` als auch
/// `std::option::Option<std::string::String>` und jede Zwischenqualifizierung,
/// weil jeweils nur das letzte Pfadsegment betrachtet wird. Typaliase lassen
/// sich zur Makro-Zeit nicht auflösen und gelten daher als nicht passend.
///
/// # Arguments
/// - `ty` (`&Type`): der zu prüfende Feldtyp.
///
/// # Returns
/// `true`, wenn der Typ als `Option<String>` erkannt wurde.
fn is_option_string(ty: &Type) -> bool {
    // Muss ein Pfadtyp sein, dessen letztes Segment `Option` heißt.
    let Type::Path(tp) = ty else { return false };
    let Some(seg) = tp.path.segments.last() else {
        return false;
    };
    if seg.ident != "Option" {
        return false;
    }
    // Genau ein generisches Typargument extrahieren.
    let syn::PathArguments::AngleBracketed(ref args) = seg.arguments else {
        return false;
    };
    let mut types = args.args.iter().filter_map(|a| {
        if let syn::GenericArgument::Type(t) = a {
            Some(t)
        } else {
            None
        }
    });
    let Some(inner) = types.next() else {
        return false;
    };
    if types.next().is_some() {
        return false; // Mehr als ein Typargument — kein einfaches Option<T>.
    }
    // Der innere Typ muss ein Pfad sein, dessen letztes Segment `String` heißt.
    let Type::Path(inner_tp) = inner else {
        return false;
    };
    inner_tp
        .path
        .segments
        .last()
        .map(|s| s.ident == "String")
        .unwrap_or(false)
}

// ── Namenskonvertierung ──────────────────────────────────────────────────────

/// Liefert den Bezeichnernamen ohne führendes `r#` roher Bezeichner.
fn ident_name(ident: &syn::Ident) -> String {
    let raw = ident.to_string();
    match raw.strip_prefix("r#") {
        Some(stripped) => stripped.to_owned(),
        None => raw,
    }
}

/// Zerlegt einen `PascalCase`-Bezeichner in kleingeschriebene Wörter.
///
/// # Description
/// Eine Wortgrenze liegt vor einem Großbuchstaben, wenn das vorherige Zeichen
/// klein oder eine Ziffer ist (`AddCriterion` → `add`, `criterion`) oder wenn
/// eine Akronymfolge endet (`HTTPGet` → `http`, `get`). `_` und `-` gelten
/// zusätzlich als explizite Trenner.
///
/// # Arguments
/// - `ident` (`&str`): Bezeichnername ohne `r#`-Präfix.
///
/// # Returns
/// Die Wortliste in Reihenfolge, jedes Wort komplett kleingeschrieben.
fn split_words(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();

    for (idx, ch) in chars.iter().enumerate() {
        // Explizite Trenner beenden das laufende Wort.
        if *ch == '_' || *ch == '-' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }

        let prev = idx.checked_sub(1).and_then(|i| chars.get(i)).copied();
        let next = chars.get(idx + 1).copied();
        // Grenze vor einem Großbuchstaben: entweder nach einem Klein-/Ziffern-
        // zeichen oder am Ende eines Akronyms (Groß, Groß, Klein).
        let is_boundary = ch.is_uppercase()
            && !current.is_empty()
            && match (prev, next) {
                (Some(p), _) if p.is_lowercase() || p.is_ascii_digit() => true,
                (Some(p), Some(n)) if p.is_uppercase() && n.is_lowercase() => true,
                _ => false,
            };
        if is_boundary {
            words.push(std::mem::take(&mut current));
        }
        current.extend(ch.to_lowercase());
    }

    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Wandelt einen `PascalCase`-Variantennamen in `kebab-case` um.
fn kebab_case(ident: &str) -> String {
    split_words(ident).join("-")
}

/// Wandelt einen `PascalCase`-Variantennamen in `snake_case` um.
fn snake_case(ident: &str) -> String {
    split_words(ident).join("_")
}

/// Liefert den Namen eines Meta-Pfads für Fehlermeldungen.
fn meta_path_name(path: &syn::Path) -> String {
    path.get_ident()
        .map(|i| i.to_string())
        .unwrap_or_else(|| "<unbekannt>".to_owned())
}

// ── Feldmodi ─────────────────────────────────────────────────────────────────

/// Ausgewerteter Modus eines `#[raw(...)]`-Feldattributs.
///
/// Alle Indizes beziehen sich auf den *Bezugs-Slice* des jeweiligen Pfads:
/// den vollen Token-Slice bei Structs, `tokens[1..]` bei Enum-Varianten.
#[derive(Debug)]
enum RawMode {
    /// `#[raw(first)]` — erstes Token des Bezugs-Slices.
    First,
    /// `#[raw(join)]` — alle Tokens mit Leerzeichen verbunden.
    Join,
    /// `#[raw(nth = N)]` — N-tes Token des Bezugs-Slices.
    Nth(usize),
    /// `#[raw(join_from = N)]` — Tokens ab Index N, mit Leerzeichen verbunden.
    JoinFrom(usize),
    /// `#[raw(required)]` — erstes Token, sonst `OpError::InvalidArguments`.
    Required,
}

impl RawMode {
    /// Liefert den Attributnamen für Fehlermeldungen.
    fn attr_name(&self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Join => "join",
            Self::Nth(_) => "nth",
            Self::JoinFrom(_) => "join_from",
            Self::Required => "required",
        }
    }
}

/// Ein analysiertes benanntes Feld mit seinem Verteilungsmodus.
struct FieldInfo {
    ident: syn::Ident,
    mode: RawMode,
}

/// Liest einen `usize`-Wert aus `key = N` innerhalb von `#[raw(...)]`.
///
/// # Returns
/// Das geparste `usize` sowie das Literal-Token (für präzise Fehler-Spans).
///
/// # Errors
/// [`syn::Error`], wenn kein Ganzzahl-Literal folgt oder es nicht in `usize` passt.
fn parse_usize_arg(
    meta: &syn::meta::ParseNestedMeta<'_>,
    key: &str,
) -> syn::Result<(usize, syn::LitInt)> {
    let lit: syn::LitInt = meta.value()?.parse().map_err(|_| {
        meta.error(format!(
            "unbekanntes oder ungültiges raw-Attribut: `{key}` erwartet ein Ganzzahl-Literal"
        ))
    })?;
    let value: usize = lit.base10_parse().map_err(|_| {
        syn::Error::new_spanned(
            &lit,
            format!(
                "unbekanntes oder ungültiges raw-Attribut: der Wert von `{key}` muss eine nicht-negative Ganzzahl sein"
            ),
        )
    })?;
    Ok((value, lit))
}

/// Analysiert alle benannten Felder eines Structs oder einer Enum-Variante.
///
/// # Description
/// Führt pro Feld sämtliche Diagnosen durch: genau ein `#[raw(...)]`-Schlüssel,
/// bekannter Schlüssel, Feldtyp `Option<String>`, und abschließend höchstens ein
/// `#[raw(required)]` im gesamten Feldsatz.
///
/// # Arguments
/// - `fields` (`&Punctuated<Field, Comma>`): die benannten Felder.
/// - `zero_based_nth` (`bool`): `true` im Enum-Pfad — dort ist `nth = 0` gültig;
///   `false` im Struct-Pfad — dort ist `nth = 0` ein Compile-Fehler.
/// - `owner` (`&proc_macro2::TokenStream`): Span-Träger für die
///   `required`-Sammeldiagnose (Struct-Input bzw. Variantenbezeichner).
/// - `scope` (`&str`): bereits formatierter Zusatz für die
///   `required`-Sammeldiagnose, z. B. `" in Variante \`Create\`"`; im Struct-Pfad
///   leer.
///
/// # Returns
/// Die Feldinfos in Deklarationsreihenfolge.
///
/// # Errors
/// [`syn::Error`] für jede der oben genannten Diagnosen.
fn parse_fields(
    fields: &Punctuated<Field, Comma>,
    zero_based_nth: bool,
    owner: &proc_macro2::TokenStream,
    scope: &str,
) -> syn::Result<Vec<FieldInfo>> {
    let mut infos: Vec<FieldInfo> = Vec::with_capacity(fields.len());
    let mut required_field_names: Vec<String> = Vec::new();

    for field in fields {
        let Some(ident) = field.ident.clone() else {
            return Err(syn::Error::new_spanned(field, STRUCT_SHAPE_MSG));
        };
        let ident_str = ident.to_string();

        // Alle gesehenen Schlüssel sammeln, um Widersprüche zu erkennen.
        let mut seen_keys: Vec<String> = Vec::new();
        let mut mode_opt: Option<RawMode> = None;

        for attr in &field.attrs {
            if !attr.path().is_ident("raw") {
                continue;
            }
            // parse_nested_meta besucht jeden kommaseparierten Schlüssel in `#[raw(...)]`.
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("first") {
                    seen_keys.push("first".to_owned());
                    mode_opt = Some(RawMode::First);
                    Ok(())
                } else if meta.path.is_ident("join") {
                    seen_keys.push("join".to_owned());
                    mode_opt = Some(RawMode::Join);
                    Ok(())
                } else if meta.path.is_ident("required") {
                    seen_keys.push("required".to_owned());
                    mode_opt = Some(RawMode::Required);
                    Ok(())
                } else if meta.path.is_ident("nth") {
                    let (n, lit) = parse_usize_arg(&meta, "nth")?;
                    // `nth = 0` ist nur im Enum-Pfad sinnvoll (0-basiert auf dem Rest).
                    if n == 0 && !zero_based_nth {
                        return Err(syn::Error::new_spanned(&lit, NTH_ZERO_MSG));
                    }
                    seen_keys.push(format!("nth = {n}"));
                    mode_opt = Some(RawMode::Nth(n));
                    Ok(())
                } else if meta.path.is_ident("join_from") {
                    // join_from ist in beiden Pfaden 0-basiert und ohne Untergrenze.
                    let (n, _lit) = parse_usize_arg(&meta, "join_from")?;
                    seen_keys.push(format!("join_from = {n}"));
                    mode_opt = Some(RawMode::JoinFrom(n));
                    Ok(())
                } else {
                    Err(meta.error(format!(
                        "unbekanntes oder ungültiges raw-Attribut: `{}`; erwartet eines von {RAW_FIELD_KEYS}",
                        meta_path_name(&meta.path),
                    )))
                }
            })?;
        }

        // Widersprüchliche Attribute am selben Feld.
        if seen_keys.len() > 1 {
            return Err(syn::Error::new_spanned(
                &ident,
                format!(
                    "widersprüchliche #[raw(...)]-Attribute an Feld '{ident_str}': {}",
                    seen_keys.join(", ")
                ),
            ));
        }

        // Fehlendes `#[raw(...)]`.
        let Some(mode) = mode_opt else {
            return Err(syn::Error::new_spanned(
                &ident,
                format!(
                    "Feld '{ident_str}' hat kein #[raw(...)]-Attribut; erwartet eines von {RAW_FIELD_KEYS}"
                ),
            ));
        };

        // Alle Modi verlangen Option<String>.
        if !is_option_string(&field.ty) {
            let attr_name = mode.attr_name();
            return Err(syn::Error::new_spanned(
                &field.ty,
                format!("#[raw({attr_name})] verlangt Option<String>"),
            ));
        }

        if matches!(mode, RawMode::Required) {
            required_field_names.push(ident_str);
        }

        infos.push(FieldInfo { ident, mode });
    }

    // Höchstens ein `required` pro Struct bzw. pro Variante.
    if required_field_names.len() > 1 {
        return Err(syn::Error::new_spanned(
            owner,
            format!(
                "höchstens ein Feld darf #[raw(required)] tragen{scope}; gefunden: {}",
                required_field_names.join(", ")
            ),
        ));
    }

    Ok(infos)
}

/// Erzeugt die Feldinitialisierer relativ zu einem Bezugs-Slice-Ausdruck.
///
/// # Arguments
/// - `infos` (`&[FieldInfo]`): analysierte Felder.
/// - `slice` (`&proc_macro2::TokenStream`): Ausdruck vom Typ `&[String]` —
///   `tokens` im Struct-Pfad, `__harw_rest` im Enum-Pfad.
///
/// # Returns
/// Je ein `ident: <ausdruck>`-Fragment pro Feld, in Deklarationsreihenfolge.
fn field_inits(
    infos: &[FieldInfo],
    slice: &proc_macro2::TokenStream,
) -> Vec<proc_macro2::TokenStream> {
    infos
        .iter()
        .map(|fi| {
            let ident = &fi.ident;
            let ident_str = ident.to_string();
            match &fi.mode {
                RawMode::First => quote! {
                    #ident: ::harw_operations::args::first_optional(#slice)
                },
                RawMode::Join => quote! {
                    #ident: ::harw_operations::args::join_all_optional(#slice)
                },
                RawMode::Nth(n) => quote! {
                    #ident: ::harw_operations::args::nth_optional(#slice, #n)
                },
                RawMode::JoinFrom(n) => quote! {
                    #ident: ::harw_operations::args::join_from(#slice, #n)
                },
                // `require_first` liefert `String`, das Feld ist `Option<String>`.
                RawMode::Required => quote! {
                    #ident: ::core::option::Option::Some(
                        ::harw_operations::args::require_first(#slice, #ident_str)?
                    )
                },
            }
        })
        .collect()
}

// ── Container-Attribute ──────────────────────────────────────────────────────

/// Prüft, ob am Typ `#[raw(subcommand)]` steht.
///
/// # Errors
/// [`syn::Error`] bei jedem anderen Schlüssel auf Container-Ebene.
fn parse_container_subcommand(input: &DeriveInput) -> syn::Result<bool> {
    let mut found = false;
    for attr in &input.attrs {
        if !attr.path().is_ident("raw") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("subcommand") {
                found = true;
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unbekanntes oder ungültiges raw-Attribut am Typ: `{}`; erlaubt ist hier nur `subcommand`",
                    meta_path_name(&meta.path),
                )))
            }
        })?;
    }
    Ok(found)
}

/// Ausgewertete `#[raw(...)]`-Attribute einer Enum-Variante.
struct VariantAttrs {
    /// Zusätzliche Erkennungs-Tokens, bereits kleingeschrieben.
    aliases: Vec<String>,
    /// `true`, wenn die Variante `#[raw(default_subcommand)]` trägt.
    is_default: bool,
}

/// Liest `alias = "…"` und `default_subcommand` einer Variante.
///
/// # Errors
/// [`syn::Error`] bei unbekanntem Schlüssel, fehlendem String-Literal, leerem
/// Alias oder einem Alias mit Leerzeichen.
fn parse_variant_attrs(variant: &Variant) -> syn::Result<VariantAttrs> {
    let mut aliases: Vec<String> = Vec::new();
    let mut is_default = false;

    for attr in &variant.attrs {
        if !attr.path().is_ident("raw") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("alias") {
                let lit: syn::LitStr = meta
                    .value()?
                    .parse()
                    .map_err(|_| meta.error("#[raw(alias = \"…\")] erwartet ein String-Literal"))?;
                let value = lit.value();
                if value.trim().is_empty() {
                    return Err(syn::Error::new_spanned(
                        &lit,
                        "#[raw(alias = \"…\")] darf nicht leer sein",
                    ));
                }
                if value.chars().any(char::is_whitespace) {
                    return Err(syn::Error::new_spanned(
                        &lit,
                        format!(
                            "#[raw(alias = \"{value}\")] darf keine Leerzeichen enthalten; ein Subcommand ist genau ein Token"
                        ),
                    ));
                }
                aliases.push(value.to_lowercase());
                Ok(())
            } else if meta.path.is_ident("default_subcommand") {
                is_default = true;
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unbekanntes oder ungültiges raw-Attribut an Variante: `{}`; erwartet eines von alias = \"…\", default_subcommand",
                    meta_path_name(&meta.path),
                )))
            }
        })?;
    }

    Ok(VariantAttrs {
        aliases,
        is_default,
    })
}

// ── Einstiegspunkt ───────────────────────────────────────────────────────────

/// Expandiert `#[derive(FromRawArgs)]` für Structs und Subcommand-Enums.
///
/// # Description
/// Wählt anhand von `input.data` und dem Container-Attribut `#[raw(subcommand)]`
/// zwischen Struct- und Enum-Pfad. Sämtliche Diagnosen laufen vor der
/// Code-Erzeugung.
///
/// # Arguments
/// - `input` (`&DeriveInput`): der geparste Typ, an dem das Derive steht.
///
/// # Returns
/// Den `impl ::harw_operations::FromRawArgs`-Block als Token-Strom.
///
/// # Errors
/// [`syn::Error`] für jede Compile-Zeit-Diagnose; siehe Modul-Doku.
pub(crate) fn expand_from_raw_args(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let has_subcommand = parse_container_subcommand(input)?;

    match &input.data {
        Data::Struct(data) => {
            if has_subcommand {
                return Err(syn::Error::new_spanned(
                    input,
                    "#[raw(subcommand)] ist nur an Enums erlaubt; Structs verteilen ihre Tokens direkt über die Feld-Attribute",
                ));
            }
            expand_struct(input, data)
        }
        Data::Enum(data) => {
            if !has_subcommand {
                return Err(syn::Error::new_spanned(
                    input,
                    "FromRawArgs an einem Enum verlangt #[raw(subcommand)] am Enum selbst",
                ));
            }
            expand_enum(input, data)
        }
        Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "FromRawArgs unterstützt nur Structs mit benannten Feldern und #[raw(subcommand)]-Enums",
        )),
    }
}

// ── Struct-Pfad ──────────────────────────────────────────────────────────────

/// Erzeugt die `FromRawArgs`-Implementierung für ein Struct mit benannten Feldern.
///
/// # Description
/// Jedes Feld greift direkt auf den vollen `tokens`-Slice zu. Structs mit null
/// Feldern übersetzen sauber und erzeugen `Self {}`.
///
/// # Errors
/// [`syn::Error`] bei Tupel-/Unit-Structs sowie bei jeder Feld-Diagnose aus
/// [`parse_fields`].
fn expand_struct(
    input: &DeriveInput,
    data: &syn::DataStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let struct_name = &input.ident;

    let fields = match &data.fields {
        Fields::Named(named) => &named.named,
        Fields::Unit | Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(input, STRUCT_SHAPE_MSG));
        }
    };

    // Struct-Pfad: `nth` ist 1-basiert, `nth = 0` bleibt ein Compile-Fehler.
    let infos = parse_fields(fields, false, &input.to_token_stream(), "")?;
    let inits = field_inits(&infos, &quote!(tokens));

    Ok(quote! {
        impl ::harw_operations::FromRawArgs for #struct_name {
            fn from_raw_args(tokens: &[::std::string::String])
                -> ::core::result::Result<Self, ::harw_operations::OpError>
            {
                ::core::result::Result::Ok(Self { #(#inits),* })
            }
        }
    })
}

// ── Enum-Pfad ────────────────────────────────────────────────────────────────

/// Eine analysierte Enum-Variante mit ihren Erkennungs-Tokens.
struct VariantInfo {
    ident: syn::Ident,
    /// Kleingeschriebene Erkennungs-Tokens: kebab-case, snake_case, Aliase.
    literals: Vec<String>,
    /// `None` bei Unit-Varianten, sonst die analysierten Felder.
    fields: Option<Vec<FieldInfo>>,
    is_default: bool,
}

/// Baut den `Self::Variant { … }`-Konstruktorausdruck einer Variante.
///
/// Unit-Varianten erzeugen `Self::Variant` ohne jede Token-Prüfung — überzählige
/// Tokens werden damit bewusst ignoriert.
fn variant_constructor(
    info: &VariantInfo,
    rest_slice: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let ident = &info.ident;
    match &info.fields {
        None => quote!(Self::#ident),
        Some(fields) => {
            let inits = field_inits(fields, rest_slice);
            quote!(Self::#ident { #(#inits),* })
        }
    }
}

/// Erzeugt die `FromRawArgs`-Implementierung für ein `#[raw(subcommand)]`-Enum.
///
/// # Description
/// Das erste Token wählt (case-insensitiv) die Variante — akzeptiert werden der
/// Variantenname in `kebab-case` und in `snake_case` sowie alle `alias`-Werte.
/// Die restlichen Tokens (`tokens[1..]`) werden über die Feld-Attribute
/// verteilt; `nth` ist dort **0-basiert relativ zum Rest**.
///
/// Fehlt das erste Token, greift `#[raw(default_subcommand)]`, sofern deklariert;
/// sonst entsteht `OpError::InvalidArguments` mit der alphabetischen
/// kebab-case-Liste aller Subcommands. Ein unbekanntes erstes Token liefert
/// dieselbe Liste, ergänzt um den unbekannten Wert.
///
/// # Errors
/// [`syn::Error`] bei leerem Enum, Tupel-Variante, doppeltem Subcommand-Token,
/// mehr als einem `default_subcommand` sowie bei jeder Feld-Diagnose.
fn expand_enum(input: &DeriveInput, data: &syn::DataEnum) -> syn::Result<proc_macro2::TokenStream> {
    let enum_name = &input.ident;

    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "FromRawArgs verlangt mindestens eine Variante am #[raw(subcommand)]-Enum",
        ));
    }

    let mut infos: Vec<VariantInfo> = Vec::with_capacity(data.variants.len());
    // Für die Usage-Liste: nur die kebab-case-Variantennamen, keine Aliase.
    let mut subcommand_names: Vec<String> = Vec::with_capacity(data.variants.len());
    // Belegte Erkennungs-Tokens -> Variantenname, zur Kollisionsprüfung.
    let mut claimed: Vec<(String, String)> = Vec::new();
    let mut defaults: Vec<String> = Vec::new();

    for variant in &data.variants {
        let VariantAttrs {
            aliases,
            is_default,
        } = parse_variant_attrs(variant)?;
        let name = ident_name(&variant.ident);
        let kebab = kebab_case(&name);

        // Erkennungs-Tokens in stabiler Reihenfolge, ohne Duplikate. Bei
        // einwortigen Varianten sind kebab und snake identisch — ohne
        // Deduplizierung entstünde ein doppeltes Literal im Or-Pattern.
        let mut literals: Vec<String> = Vec::with_capacity(2 + aliases.len());
        for candidate in [kebab.clone(), snake_case(&name)]
            .into_iter()
            .chain(aliases)
        {
            if !literals.contains(&candidate) {
                literals.push(candidate);
            }
        }

        // Zwei Varianten dürfen sich kein Token teilen — der zweite Match-Arm
        // wäre sonst unerreichbar und die Auswahl hinge an der
        // Deklarationsreihenfolge.
        for literal in &literals {
            if let Some((_, owner)) = claimed.iter().find(|(l, _)| l == literal) {
                return Err(syn::Error::new_spanned(
                    &variant.ident,
                    format!(
                        "doppelter Subcommand-Token `{literal}`: Variante `{name}` kollidiert mit Variante `{owner}`"
                    ),
                ));
            }
            claimed.push((literal.clone(), name.clone()));
        }

        let fields = match &variant.fields {
            Fields::Unit => None,
            Fields::Named(named) => Some(parse_fields(
                &named.named,
                true, // Enum-Pfad: `nth` ist 0-basiert relativ zum Rest.
                &variant.ident.to_token_stream(),
                &format!(" in Variante `{name}`"),
            )?),
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    variant,
                    format!(
                        "FromRawArgs unterstützt nur Unit- und Struct-Varianten; die Tupel-Variante `{name}` ist nicht erlaubt"
                    ),
                ));
            }
        };

        if is_default {
            defaults.push(name.clone());
        }
        subcommand_names.push(kebab);
        infos.push(VariantInfo {
            ident: variant.ident.clone(),
            literals,
            fields,
            is_default,
        });
    }

    if defaults.len() > 1 {
        return Err(syn::Error::new_spanned(
            input,
            format!(
                "höchstens eine Variante darf #[raw(default_subcommand)] tragen; gefunden: {}",
                defaults.join(", ")
            ),
        ));
    }

    // Usage-Liste: kebab-case, alphabetisch, ohne Aliase.
    subcommand_names.sort();
    let subcommand_list = subcommand_names.join(", ");
    let rest_slice = quote!(__harw_rest);

    let arms: Vec<proc_macro2::TokenStream> = infos
        .iter()
        .map(|info| {
            let literals = &info.literals;
            let ctor = variant_constructor(info, &rest_slice);
            quote! {
                ::core::option::Option::Some(#(#literals)|*) => ::core::result::Result::Ok(#ctor),
            }
        })
        .collect();

    // Kein Token vorhanden: entweder default_subcommand oder Usage-Fehler.
    let none_arm = match infos.iter().find(|info| info.is_default) {
        Some(info) => {
            let ctor = variant_constructor(info, &rest_slice);
            quote! {
                ::core::option::Option::None => ::core::result::Result::Ok(#ctor),
            }
        }
        None => {
            let msg = format!("kein Subcommand angegeben; erwartet eines von: {subcommand_list}");
            quote! {
                ::core::option::Option::None => ::core::result::Result::Err(
                    ::harw_operations::OpError::InvalidArguments(
                        ::std::string::String::from(#msg)
                    )
                ),
            }
        }
    };

    Ok(quote! {
        impl ::harw_operations::FromRawArgs for #enum_name {
            fn from_raw_args(tokens: &[::std::string::String])
                -> ::core::result::Result<Self, ::harw_operations::OpError>
            {
                // Originaltoken für die Fehlermeldung, kleingeschriebene Kopie
                // für den case-insensitiven Vergleich.
                let __harw_head_raw: ::core::option::Option<&::std::string::String> = tokens.first();
                let __harw_head: ::core::option::Option<::std::string::String> =
                    __harw_head_raw.map(|t| t.to_lowercase());
                // Alles nach dem Subcommand; bei leerem Input der leere Slice.
                let __harw_rest: &[::std::string::String] = if tokens.is_empty() {
                    tokens
                } else {
                    &tokens[1..]
                };

                match __harw_head.as_deref() {
                    #none_arm
                    #(#arms)*
                    ::core::option::Option::Some(_) => ::core::result::Result::Err(
                        ::harw_operations::OpError::InvalidArguments(
                            ::std::format!(
                                "unbekannter Subcommand `{}`; erwartet eines von: {}",
                                __harw_head_raw.map(|t| t.as_str()).unwrap_or(""),
                                #subcommand_list,
                            )
                        )
                    ),
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use syn::DeriveInput;

    // ── Helfer ────────────────────────────────────────────────────────────────

    /// Expandiert und liefert den Token-Strom als String.
    fn expand_ok(input: &DeriveInput) -> TestResult<String> {
        Ok(expand_from_raw_args(input)
            .map_err(ctx("Expansion muss gelingen"))?
            .to_string())
    }

    /// Expandiert und liefert die Fehlermeldung.
    fn expand_err(input: &DeriveInput) -> TestResult<String> {
        let Err(error) = expand_from_raw_args(input) else {
            return Err(TestError::Unexpected(
                "Expansion muss fehlschlagen".to_owned(),
            ));
        };
        Ok(error.to_string())
    }

    /// Entfernt jeden Leerraum, damit Code-Form-Zusicherungen unabhängig von der
    /// Token-Zwischenraum-Formatierung von `proc_macro2` sind.
    fn squeeze(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// Beispiel-Enum, das alle Enum-Features abdeckt.
    fn plan_args() -> DeriveInput {
        syn::parse_quote! {
            #[raw(subcommand)]
            pub enum PlanArgs {
                Create {
                    #[raw(nth = 0)]
                    id: Option<String>,
                    #[raw(join_from = 1)]
                    goal: Option<String>,
                },
                Ready,
                Status {
                    #[raw(nth = 0)]
                    id: Option<String>,
                    #[raw(nth = 1)]
                    status: Option<String>,
                },
                #[raw(alias = "ls")]
                Inspect,
                AddCriterion {
                    #[raw(join)]
                    text: Option<String>,
                },
            }
        }
    }

    // ── Namenskonvertierung ──────────────────────────────────────────────────

    #[test]
    fn test_kebab_case_splits_pascal_words() {
        assert_eq!(kebab_case("AddCriterion"), "add-criterion");
        assert_eq!(kebab_case("Ready"), "ready");
        assert_eq!(kebab_case("HTTPGet"), "http-get");
        assert_eq!(kebab_case("Plan2Goal"), "plan2-goal");
    }

    #[test]
    fn test_snake_case_splits_pascal_words() {
        assert_eq!(snake_case("AddCriterion"), "add_criterion");
        assert_eq!(snake_case("Ready"), "ready");
        assert_eq!(snake_case("HTTPGet"), "http_get");
    }

    #[test]
    fn test_ident_name_strips_raw_prefix() {
        let ident = syn::Ident::new_raw("type", proc_macro2::Span::call_site());
        assert_eq!(ident_name(&ident), "type");
    }

    // ── Variantenauswahl ─────────────────────────────────────────────────────

    #[test]
    fn test_enum_variant_matches_kebab_and_snake_case() -> TestResult {
        let expanded = squeeze(&expand_ok(&plan_args())?);
        assert!(
            expanded.contains(r#"Some("add-criterion"|"add_criterion")"#),
            "kebab- und snake_case-Token müssen im selben Or-Pattern stehen: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_single_word_variant_emits_token_only_once() -> TestResult {
        let expanded = squeeze(&expand_ok(&plan_args())?);
        // `Ready` ist in kebab und snake identisch — ohne Deduplizierung würde
        // `unreachable_patterns` anschlagen.
        assert!(
            expanded.contains(r#"Some("ready")=>"#),
            "erwartetes Einzel-Literal fehlt: {expanded}"
        );
        assert!(
            !expanded.contains(r#""ready"|"ready""#),
            "Literal wurde nicht dedupliziert: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_alias_is_accepted_as_extra_token() -> TestResult {
        let expanded = squeeze(&expand_ok(&plan_args())?);
        assert!(
            expanded.contains(r#"Some("inspect"|"ls")"#),
            "Alias fehlt im Or-Pattern: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_head_comparison_is_case_insensitive() -> TestResult {
        let expanded = expand_ok(&plan_args())?;
        // Der Kopf wird kleingeschrieben, die Pattern-Literale sind bereits klein.
        assert!(
            expanded.contains("to_lowercase"),
            "case-insensitiver Vergleich fehlt: {expanded}"
        );
        assert!(
            !expanded.contains(r#""Create""#),
            "Pattern-Literal wurde nicht kleingeschrieben: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_alias_is_lowercased_at_expansion_time() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                #[raw(alias = "LS")]
                Inspect,
            }
        };
        let expanded = squeeze(&expand_ok(&input)?);
        assert!(
            expanded.contains(r#"Some("inspect"|"ls")"#),
            "Alias wurde nicht kleingeschrieben: {expanded}"
        );
        Ok(())
    }

    // ── Feldverteilung relativ zum Rest ──────────────────────────────────────

    #[test]
    fn test_enum_nth_is_zero_based_relative_to_rest() -> TestResult {
        let expanded = squeeze(&expand_ok(&plan_args())?);
        // `nth = 0` im Enum meint das Token direkt nach dem Subcommand.
        assert!(
            expanded.contains("nth_optional(__harw_rest,0usize)"),
            "nth = 0 muss auf __harw_rest zeigen: {expanded}"
        );
        assert!(
            expanded.contains("nth_optional(__harw_rest,1usize)"),
            "nth = 1 muss auf __harw_rest zeigen: {expanded}"
        );
        // Der Rest-Slice überspringt genau ein Token.
        assert!(
            expanded.contains("&tokens[1..]"),
            "__harw_rest muss tokens[1..] sein: {expanded}"
        );
        // Kein Feld darf im Enum-Pfad den vollen Slice sehen.
        assert!(
            !expanded.contains("nth_optional(tokens,"),
            "Enum-Felder dürfen nicht auf den vollen Slice zeigen: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_join_from_uses_rest_slice() -> TestResult {
        let expanded = squeeze(&expand_ok(&plan_args())?);
        assert!(
            expanded.contains("join_from(__harw_rest,1usize)"),
            "join_from muss auf __harw_rest zeigen: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_join_uses_rest_slice() -> TestResult {
        let expanded = squeeze(&expand_ok(&plan_args())?);
        assert!(
            expanded.contains("join_all_optional(__harw_rest)"),
            "join muss auf __harw_rest zeigen: {expanded}"
        );
        Ok(())
    }

    // ── Unit-Varianten ───────────────────────────────────────────────────────

    #[test]
    fn test_enum_unit_variant_ignores_extra_tokens() -> TestResult {
        let expanded = squeeze(&expand_ok(&plan_args())?);
        // Unit-Varianten konstruieren ohne Token-Prüfung — `/plan ready --verbose`
        // darf nicht scheitern.
        assert!(
            expanded.contains("Ok(Self::Ready)"),
            "Unit-Variante muss ohne Feldliste konstruiert werden: {expanded}"
        );
        assert!(
            !expanded.contains("require_empty"),
            "Unit-Variante darf überzählige Tokens nicht ablehnen: {expanded}"
        );
        Ok(())
    }

    // ── Usage-Meldungen ──────────────────────────────────────────────────────

    #[test]
    fn test_enum_unknown_subcommand_lists_all_subcommands_alphabetically() -> TestResult {
        let expanded = expand_ok(&plan_args())?;
        assert!(
            expanded.contains("unbekannter Subcommand `{}`; erwartet eines von: {}"),
            "Fehlertext für unbekannten Subcommand fehlt: {expanded}"
        );
        // kebab-case, alphabetisch, ohne Aliase.
        assert!(
            expanded.contains(r#""add-criterion, create, inspect, ready, status""#),
            "Subcommand-Liste fehlt oder ist unsortiert: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_missing_subcommand_lists_all_subcommands() -> TestResult {
        let expanded = expand_ok(&plan_args())?;
        assert!(
            expanded.contains(
                r#""kein Subcommand angegeben; erwartet eines von: add-criterion, create, inspect, ready, status""#
            ),
            "Fehlertext für fehlenden Subcommand fehlt: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_usage_list_excludes_aliases() -> TestResult {
        let expanded = expand_ok(&plan_args())?;
        assert!(
            !expanded.contains("inspect, ls"),
            "Aliase dürfen nicht in der Usage-Liste stehen: {expanded}"
        );
        Ok(())
    }

    // ── default_subcommand ───────────────────────────────────────────────────

    #[test]
    fn test_enum_default_subcommand_handles_empty_input() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum ModeArgs {
                #[raw(default_subcommand)]
                Show,
                Set { #[raw(nth = 0)] value: Option<String> },
            }
        };
        let expanded = squeeze(&expand_ok(&input)?);
        assert!(
            expanded.contains("Option::None=>::core::result::Result::Ok(Self::Show)"),
            "leerer Input muss die Default-Variante liefern: {expanded}"
        );
        assert!(
            !expanded.contains("keinSubcommandangegeben"),
            "mit default_subcommand darf kein Usage-Fehler erzeugt werden: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_default_subcommand_with_fields_gets_empty_rest() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum GoalArgs {
                #[raw(default_subcommand)]
                Show { #[raw(join_from = 0)] filter: Option<String> },
            }
        };
        let expanded = squeeze(&expand_ok(&input)?);
        assert!(
            expanded.contains("Option::None=>::core::result::Result::Ok(Self::Show{"),
            "Default-Variante mit Feldern fehlt: {expanded}"
        );
        assert!(
            expanded.contains("join_from(__harw_rest,0usize)"),
            "Default-Variante muss ebenfalls __harw_rest nutzen: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_two_default_subcommands_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                #[raw(default_subcommand)]
                Show,
                #[raw(default_subcommand)]
                List,
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "höchstens eine Variante darf #[raw(default_subcommand)] tragen; gefunden: Show, List"
        );
        Ok(())
    }

    // ── Compile-Fehler im Enum-Pfad ──────────────────────────────────────────

    #[test]
    fn test_enum_tuple_variant_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                Create(String),
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "FromRawArgs unterstützt nur Unit- und Struct-Varianten; die Tupel-Variante `Create` ist nicht erlaubt"
        );
        Ok(())
    }

    #[test]
    fn test_enum_without_subcommand_attribute_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            enum E {
                Ready,
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "FromRawArgs an einem Enum verlangt #[raw(subcommand)] am Enum selbst"
        );
        Ok(())
    }

    #[test]
    fn test_enum_without_variants_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {}
        };
        assert_eq!(
            expand_err(&input)?,
            "FromRawArgs verlangt mindestens eine Variante am #[raw(subcommand)]-Enum"
        );
        Ok(())
    }

    #[test]
    fn test_enum_duplicate_subcommand_token_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                Ready,
                #[raw(alias = "ready")]
                Start,
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "doppelter Subcommand-Token `ready`: Variante `Start` kollidiert mit Variante `Ready`"
        );
        Ok(())
    }

    #[test]
    fn test_enum_alias_with_whitespace_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                #[raw(alias = "add crit")]
                AddCriterion,
            }
        };
        let err = expand_err(&input)?;
        assert!(
            err.contains("darf keine Leerzeichen enthalten"),
            "unerwartete Meldung: {err}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_unknown_variant_key_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                #[raw(rename = "x")]
                Ready,
            }
        };
        let err = expand_err(&input)?;
        assert!(
            err.contains("unbekanntes oder ungültiges raw-Attribut an Variante: `rename`"),
            "unerwartete Meldung: {err}"
        );
        Ok(())
    }

    #[test]
    fn test_enum_multiple_required_per_variant_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                Create {
                    #[raw(required)]
                    a: Option<String>,
                    #[raw(required)]
                    b: Option<String>,
                },
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "höchstens ein Feld darf #[raw(required)] tragen in Variante `Create`; gefunden: a, b"
        );
        Ok(())
    }

    #[test]
    fn test_enum_field_wrong_type_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                Create {
                    #[raw(nth = 0)]
                    id: String,
                },
            }
        };
        assert_eq!(expand_err(&input)?, "#[raw(nth)] verlangt Option<String>");
        Ok(())
    }

    #[test]
    fn test_enum_field_missing_attribute_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                Create {
                    id: Option<String>,
                },
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "Feld 'id' hat kein #[raw(...)]-Attribut; erwartet eines von first, join, join_from = N, nth = N, required"
        );
        Ok(())
    }

    #[test]
    fn test_enum_conflicting_field_attributes_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            enum E {
                Create {
                    #[raw(first, join_from = 2)]
                    id: Option<String>,
                },
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "widersprüchliche #[raw(...)]-Attribute an Feld 'id': first, join_from = 2"
        );
        Ok(())
    }

    // ── Struct-Pfad: Rückwärtskompatibilität + join_from ─────────────────────

    #[test]
    fn test_struct_modes_reference_full_token_slice() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct SkillsArgs {
                #[raw(first)]
                action: Option<String>,
                #[raw(nth = 1)]
                name: Option<String>,
                #[raw(join)]
                all: Option<String>,
            }
        };
        let expanded = squeeze(&expand_ok(&input)?);
        assert!(expanded.contains("first_optional(tokens)"), "{expanded}");
        assert!(
            expanded.contains("nth_optional(tokens,1usize)"),
            "{expanded}"
        );
        assert!(expanded.contains("join_all_optional(tokens)"), "{expanded}");
        assert!(
            !expanded.contains("__harw_rest"),
            "Struct-Pfad darf keinen Rest-Slice erzeugen: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_struct_join_from_is_supported_and_zero_based() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Args {
                #[raw(first)]
                head: Option<String>,
                #[raw(join_from = 1)]
                tail: Option<String>,
                #[raw(join_from = 0)]
                everything: Option<String>,
            }
        };
        let expanded = squeeze(&expand_ok(&input)?);
        assert!(expanded.contains("join_from(tokens,1usize)"), "{expanded}");
        assert!(
            expanded.contains("join_from(tokens,0usize)"),
            "join_from = 0 muss im Struct-Pfad erlaubt sein: {expanded}"
        );
        Ok(())
    }

    #[test]
    fn test_struct_nth_zero_is_error_and_explains_enum_semantics() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Args {
                #[raw(nth = 0)]
                id: Option<String>,
            }
        };
        let err = expand_err(&input)?;
        assert!(
            err.contains("#[raw(nth = 0)] ist im Struct-Pfad nicht erlaubt"),
            "unerwartete Meldung: {err}"
        );
        assert!(
            err.contains("0-basiert relativ zu den Tokens nach dem Subcommand"),
            "der Semantik-Unterschied muss im Fehlertext stehen: {err}"
        );
        Ok(())
    }

    #[test]
    fn test_struct_with_subcommand_attribute_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommand)]
            struct Args {
                #[raw(first)]
                id: Option<String>,
            }
        };
        assert_eq!(
            expand_err(&input)?,
            "#[raw(subcommand)] ist nur an Enums erlaubt; Structs verteilen ihre Tokens direkt über die Feld-Attribute"
        );
        Ok(())
    }

    #[test]
    fn test_struct_tuple_shape_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Args(String);
        };
        assert_eq!(
            expand_err(&input)?,
            "FromRawArgs verlangt ein Struct mit benannten Feldern"
        );
        Ok(())
    }

    #[test]
    fn test_struct_without_fields_expands_to_empty_constructor() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Args {}
        };
        let expanded = squeeze(&expand_ok(&input)?);
        assert!(expanded.contains("Ok(Self{})"), "{expanded}");
        Ok(())
    }

    #[test]
    fn test_struct_required_wraps_value_in_some() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            struct Args {
                #[raw(required)]
                id: Option<String>,
            }
        };
        let expanded = squeeze(&expand_ok(&input)?);
        // Der Feldtyp ist Option<String>, `require_first` liefert String — der
        // Wert muss deshalb in Some(...) verpackt werden.
        assert!(
            expanded.contains(r#"Some(::harw_operations::args::require_first(tokens,"id")?)"#),
            "{expanded}"
        );
        Ok(())
    }

    // ── Container-Diagnosen ──────────────────────────────────────────────────

    #[test]
    fn test_unknown_container_key_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[raw(subcommands)]
            enum E { Ready }
        };
        let err = expand_err(&input)?;
        assert!(
            err.contains("unbekanntes oder ungültiges raw-Attribut am Typ: `subcommands`"),
            "unerwartete Meldung: {err}"
        );
        Ok(())
    }

    #[test]
    fn test_union_is_error() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            union U { a: u32 }
        };
        assert_eq!(
            expand_err(&input)?,
            "FromRawArgs unterstützt nur Structs mit benannten Feldern und #[raw(subcommand)]-Enums"
        );
        Ok(())
    }
}
