//! `metrics!`-Expansion — Metrikschlüssel-Registrierung für `harw-observe`.
//!
//! Spec: AW0-02-Brief, Abschnitt 2 (`metrics!`); Vertragsabschnitt A.2
//! (`harw-observe`, `MetricKey`/`MetricKind`/`Unit`/`Cardinality`) in
//! `docs/design/build-history.md`.
//!
//! # Grammatik
//!
//! ```ignore
//! harw_macros::metrics! {
//!     /// Doc-Kommentar, wird auf die erzeugte `const` übertragen.
//!     NAME: counter, unit = count, labels = ["clan", "role"], cardinality = bounded(64),
//!         name = "harw_child_admitted_total";
//!     ...
//! }
//! ```
//!
//! `kind` ist eines von `counter`, `gauge`, `histogram`. Die vier Schlüssel
//! `unit`, `labels`, `cardinality`, `name` müssen je genau einmal vorkommen
//! (Reihenfolge nach `kind` beliebig); ein unbekannter oder fehlender
//! Schlüssel ist ein Compile-Fehler.
//!
//! # Namensregeln (siehe [`validate_metric_name`])
//!
//! Diese Prüfungen laufen im Makro statt erst im Prometheus-Sink (AW3-04),
//! damit ein falscher Metrikname den Build bricht statt erst einen
//! Golden-Test drei Wellen später:
//!
//! - ein `counter` muss auf `_total` enden,
//! - die Einheit muss dort, wo sie eine Basiseinheit hat (`bytes`, `seconds`,
//!   `tokens`, `celsius`), als Teilstring (`_bytes`/`_seconds`/`_tokens`/
//!   `_celsius`) im Namen erscheinen; `count` und `ratio` verlangen keinen
//!   Suffix,
//! - der Name besteht ausschließlich aus `[a-z0-9_]`.
//!
//! `labels`-Einträge werden mit derselben Regel wie `field!` geprüft (siehe
//! [`crate::field::validate_field_name`]).
//!
//! # Erzeugte API
//!
//! Je Eintrag eine `pub const <NAME>: ::harw_observe::MetricKey`, plus ein
//! abschließendes `pub static ALL: &[&::harw_observe::MetricKey]` mit allen
//! Einträgen dieser Deklaration in Reihenfolge.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Ident, LitInt, LitStr, Token};

use crate::field::validate_field_name;

/// Die Art der Messgröße, textuell aus dem `kind`-Bezeichner geparst.
///
/// Unabhängig vom `MetricKind` aus `harw-observe`: `harw-macros` hängt nicht
/// von `harw-observe` ab (siehe Modul-Doc von `lib.rs`), daher spiegelt
/// dieser Typ nur die für Codegen und Namensvalidierung nötige Information
/// wider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedKind {
    Counter,
    Gauge,
    Histogram,
}

impl ParsedKind {
    /// Erzeugt den `::harw_observe::MetricKind`-Pfad für diese Art.
    fn to_tokens(self) -> TokenStream {
        match self {
            ParsedKind::Counter => quote! { ::harw_observe::MetricKind::Counter },
            ParsedKind::Gauge => quote! { ::harw_observe::MetricKind::Gauge },
            ParsedKind::Histogram => quote! { ::harw_observe::MetricKind::Histogram },
        }
    }
}

/// Parst einen `kind`-Bezeichner (`counter`/`gauge`/`histogram`).
///
/// # Errors
/// Liefert `syn::Error`, wenn `ident` keiner der drei erlaubten Werte ist.
fn parse_kind(ident: &Ident) -> syn::Result<ParsedKind> {
    match ident.to_string().as_str() {
        "counter" => Ok(ParsedKind::Counter),
        "gauge" => Ok(ParsedKind::Gauge),
        "histogram" => Ok(ParsedKind::Histogram),
        other => Err(syn::Error::new_spanned(
            ident,
            format!("unbekannte Metrikart `{other}`; erwartet: counter, gauge, histogram"),
        )),
    }
}

/// Die Basiseinheit, textuell aus dem `unit`-Bezeichner geparst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedUnit {
    Count,
    Bytes,
    Seconds,
    Ratio,
    Tokens,
    Celsius,
}

impl ParsedUnit {
    /// Erzeugt den `::harw_observe::Unit`-Pfad für diese Einheit.
    fn to_tokens(self) -> TokenStream {
        match self {
            ParsedUnit::Count => quote! { ::harw_observe::Unit::Count },
            ParsedUnit::Bytes => quote! { ::harw_observe::Unit::Bytes },
            ParsedUnit::Seconds => quote! { ::harw_observe::Unit::Seconds },
            ParsedUnit::Ratio => quote! { ::harw_observe::Unit::Ratio },
            ParsedUnit::Tokens => quote! { ::harw_observe::Unit::Tokens },
            ParsedUnit::Celsius => quote! { ::harw_observe::Unit::Celsius },
        }
    }

    /// Der Teilstring, den der Metrikname enthalten muss — `None`, wenn diese
    /// Einheit keine Basiseinheit mit Namenssuffix hat (`count`, `ratio`).
    fn required_substring(self) -> Option<&'static str> {
        match self {
            ParsedUnit::Bytes => Some("_bytes"),
            ParsedUnit::Seconds => Some("_seconds"),
            ParsedUnit::Tokens => Some("_tokens"),
            ParsedUnit::Celsius => Some("_celsius"),
            ParsedUnit::Count | ParsedUnit::Ratio => None,
        }
    }
}

/// Parst einen `unit`-Bezeichner.
///
/// # Errors
/// Liefert `syn::Error`, wenn `ident` keiner der sechs erlaubten Werte ist.
fn parse_unit(ident: &Ident) -> syn::Result<ParsedUnit> {
    match ident.to_string().as_str() {
        "count" => Ok(ParsedUnit::Count),
        "bytes" => Ok(ParsedUnit::Bytes),
        "seconds" => Ok(ParsedUnit::Seconds),
        "ratio" => Ok(ParsedUnit::Ratio),
        "tokens" => Ok(ParsedUnit::Tokens),
        "celsius" => Ok(ParsedUnit::Celsius),
        other => Err(syn::Error::new_spanned(
            ident,
            format!(
                "unbekannte Einheit `{other}`; erwartet: count, bytes, seconds, ratio, tokens, celsius"
            ),
        )),
    }
}

/// Die Kardinalitätsgrenze, geparst aus `single` oder `bounded(N)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedCardinality {
    Bounded(u32),
    Single,
}

impl ParsedCardinality {
    /// Erzeugt den `::harw_observe::Cardinality`-Pfad für diesen Wert.
    fn to_tokens(self) -> TokenStream {
        match self {
            ParsedCardinality::Bounded(n) => quote! { ::harw_observe::Cardinality::Bounded(#n) },
            ParsedCardinality::Single => quote! { ::harw_observe::Cardinality::Single },
        }
    }
}

/// Ein geparster `metrics!`-Eintrag (eine Zeile der Deklaration).
struct MetricEntry {
    /// `///`-Doc-Kommentare vor dem Eintrag; werden unverändert übertragen.
    docs: Vec<Attribute>,
    /// Name der erzeugten `pub const`.
    ident: Ident,
    kind: ParsedKind,
    unit: ParsedUnit,
    labels: Vec<LitStr>,
    cardinality: ParsedCardinality,
    name: LitStr,
}

impl Parse for MetricEntry {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let docs = input.call(Attribute::parse_outer)?;
        let ident: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let kind_ident: Ident = input.parse()?;
        let kind = parse_kind(&kind_ident)?;

        let mut unit: Option<ParsedUnit> = None;
        let mut labels: Option<Vec<LitStr>> = None;
        let mut cardinality: Option<ParsedCardinality> = None;
        let mut name: Option<LitStr> = None;

        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "unit" => {
                    if unit.is_some() {
                        return Err(syn::Error::new_spanned(&key, "`unit` doppelt angegeben"));
                    }
                    let unit_ident: Ident = input.parse()?;
                    unit = Some(parse_unit(&unit_ident)?);
                }
                "labels" => {
                    if labels.is_some() {
                        return Err(syn::Error::new_spanned(&key, "`labels` doppelt angegeben"));
                    }
                    let content;
                    syn::bracketed!(content in input);
                    // `LitStr::parse` ist die **inhärente** Methode, die den Inhalt eines
                    // String-Literals parst — nicht `Parse::parse`. Sie verdeckt die
                    // Trait-Methode bei Pfadschreibweise, und `parse_terminated` will
                    // genau die Trait-Methode. Ohne die Qualifikation ist es ein
                    // Typfehler, den erst rustc meldet.
                    let list = content
                        .parse_terminated(<LitStr as syn::parse::Parse>::parse, Token![,])?;
                    labels = Some(list.into_iter().collect());
                }
                "cardinality" => {
                    if cardinality.is_some() {
                        return Err(syn::Error::new_spanned(
                            &key,
                            "`cardinality` doppelt angegeben",
                        ));
                    }
                    let card_ident: Ident = input.parse()?;
                    let parsed = match card_ident.to_string().as_str() {
                        "single" => ParsedCardinality::Single,
                        "bounded" => {
                            let content;
                            syn::parenthesized!(content in input);
                            let lit: LitInt = content.parse()?;
                            let n: u32 = lit.base10_parse()?;
                            ParsedCardinality::Bounded(n)
                        }
                        other => {
                            return Err(syn::Error::new_spanned(
                                &card_ident,
                                format!(
                                    "unbekannte cardinality `{other}`; erwartet: single, bounded(N)"
                                ),
                            ));
                        }
                    };
                    cardinality = Some(parsed);
                }
                "name" => {
                    if name.is_some() {
                        return Err(syn::Error::new_spanned(&key, "`name` doppelt angegeben"));
                    }
                    name = Some(input.parse()?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!(
                            "unbekannter Schlüssel `{other}`; erwartet: unit, labels, cardinality, name"
                        ),
                    ));
                }
            }
        }

        input.parse::<Token![;]>()?;

        let unit = unit.ok_or_else(|| syn::Error::new_spanned(&ident, "fehlendes `unit = ...`"))?;
        let labels =
            labels.ok_or_else(|| syn::Error::new_spanned(&ident, "fehlendes `labels = [...]`"))?;
        let cardinality = cardinality
            .ok_or_else(|| syn::Error::new_spanned(&ident, "fehlendes `cardinality = ...`"))?;
        let name =
            name.ok_or_else(|| syn::Error::new_spanned(&ident, "fehlendes `name = \"...\"`"))?;

        Ok(MetricEntry {
            docs,
            ident,
            kind,
            unit,
            labels,
            cardinality,
            name,
        })
    }
}

/// Eine vollständige `metrics! { ... }`-Deklaration: null oder mehr Einträge.
struct MetricsInput {
    entries: Vec<MetricEntry>,
}

impl Parse for MetricsInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut entries = Vec::new();
        while !input.is_empty() {
            entries.push(input.parse()?);
        }
        Ok(MetricsInput { entries })
    }
}

/// Prüft einen Metriknamen gegen die in der Modul-Doc beschriebenen Regeln.
///
/// # Errors
/// Liefert eine deutschsprachige Fehlerbeschreibung, wenn `name` leer ist,
/// ein Zeichen außerhalb `[a-z0-9_]` enthält, als `counter` nicht auf
/// `_total` endet, oder die Basiseinheit von `unit` nicht als Teilstring
/// enthält (sofern `unit` eine Basiseinheit hat).
///
/// # Design-doc reference
/// AW0-02-Brief, Abschnitt 2 (`metrics!` — Namensregeln).
fn validate_metric_name(name: &str, kind: ParsedKind, unit: ParsedUnit) -> Result<(), String> {
    if name.is_empty() {
        return Err("Metrikname darf nicht leer sein".to_owned());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '_'))
    {
        return Err(format!(
            "Metrikname `{name}` enthält ungültiges Zeichen '{c}'; erlaubt sind nur [a-z0-9_]"
        ));
    }
    if kind == ParsedKind::Counter && !name.ends_with("_total") {
        return Err(format!("counter-Metrik `{name}` muss auf `_total` enden"));
    }
    if let Some(suffix) = unit.required_substring() {
        if !name.contains(suffix) {
            return Err(format!(
                "Metrikname `{name}` mit dieser Einheit muss `{suffix}` im Namen enthalten"
            ));
        }
    }
    Ok(())
}

/// Expander für das `metrics!`-Makro.
///
/// Parst die Deklaration (siehe Modul-Doc, Abschnitt "Grammatik"), prüft
/// doppelte Konstantennamen, validiert jeden `labels`-Eintrag über
/// [`crate::field::validate_field_name`] und jeden Metriknamen über
/// [`validate_metric_name`], und erzeugt je Eintrag eine `pub const` sowie
/// ein abschließendes `pub static ALL`.
///
/// # Errors
/// - Syntaxfehler in der Deklaration → `syn::Error` (von `syn`s Parser).
/// - zwei Einträge mit demselben Konstantennamen → `syn::Error`.
/// - ein `labels`-Eintrag ist kein gültiger Feldname → `syn::Error`.
/// - der Metrikname verletzt eine Namensregel → `syn::Error`.
///
/// # Design-doc reference
/// AW0-02-Brief, Abschnitt 2 (`metrics!`).
pub(crate) fn expand_metrics(input: TokenStream) -> syn::Result<TokenStream> {
    let parsed: MetricsInput = syn::parse2(input)?;

    let mut seen: HashSet<String> = HashSet::new();
    for entry in &parsed.entries {
        let key = entry.ident.to_string();
        if !seen.insert(key.clone()) {
            return Err(syn::Error::new_spanned(
                &entry.ident,
                format!("Metrik `{key}` ist in dieser Deklaration bereits doppelt vergeben"),
            ));
        }
    }

    let mut const_items: Vec<TokenStream> = Vec::with_capacity(parsed.entries.len());
    let mut all_refs: Vec<TokenStream> = Vec::with_capacity(parsed.entries.len());

    for entry in &parsed.entries {
        for label in &entry.labels {
            validate_field_name(&label.value())
                .map_err(|reason| syn::Error::new_spanned(label, reason))?;
        }
        validate_metric_name(&entry.name.value(), entry.kind, entry.unit)
            .map_err(|reason| syn::Error::new_spanned(&entry.name, reason))?;

        let ident = &entry.ident;
        let docs = &entry.docs;
        let kind_tokens = entry.kind.to_tokens();
        let unit_tokens = entry.unit.to_tokens();
        let cardinality_tokens = entry.cardinality.to_tokens();
        let name_lit = &entry.name;
        let label_tokens: Vec<TokenStream> = entry
            .labels
            .iter()
            .map(|l| quote! { ::harw_observe::FieldName::from_static_unchecked(#l) })
            .collect();

        const_items.push(quote! {
            #(#docs)*
            pub const #ident: ::harw_observe::MetricKey = ::harw_observe::MetricKey {
                name: #name_lit,
                kind: #kind_tokens,
                unit: #unit_tokens,
                labels: &[#(#label_tokens),*],
                cardinality: #cardinality_tokens,
            };
        });
        all_refs.push(quote! { &#ident });
    }

    Ok(quote! {
        #(#const_items)*

        /// Registrierungs-Slice aller in dieser Deklaration erzeugten Metrikschlüssel.
        pub static ALL: &[&::harw_observe::MetricKey] = &[#(#all_refs),*];
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn validate_metric_name_rejects_empty() -> TestResult {
        let Err(err) = validate_metric_name("", ParsedKind::Gauge, ParsedUnit::Count) else {
            return Err(TestError::Unexpected(
                "empty name must be rejected".to_owned(),
            ));
        };
        assert!(err.contains("darf nicht leer sein"));
        Ok(())
    }

    #[test]
    fn validate_metric_name_rejects_uppercase() -> TestResult {
        let Err(err) = validate_metric_name("Harw_Cost", ParsedKind::Gauge, ParsedUnit::Count)
        else {
            return Err(TestError::Unexpected(
                "uppercase must be rejected".to_owned(),
            ));
        };
        assert!(err.contains("ungültiges Zeichen"));
        Ok(())
    }

    #[test]
    fn validate_metric_name_rejects_counter_without_total_suffix() -> TestResult {
        let Err(err) = validate_metric_name(
            "harw_child_admitted",
            ParsedKind::Counter,
            ParsedUnit::Count,
        ) else {
            return Err(TestError::Unexpected(
                "counter without _total must be rejected".to_owned(),
            ));
        };
        assert!(err.contains("_total"));
        Ok(())
    }

    #[test]
    fn validate_metric_name_accepts_counter_with_total_suffix() {
        assert!(
            validate_metric_name(
                "harw_child_admitted_total",
                ParsedKind::Counter,
                ParsedUnit::Count
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_metric_name_rejects_missing_unit_substring() -> TestResult {
        let Err(err) = validate_metric_name(
            "harw_data_read_total",
            ParsedKind::Counter,
            ParsedUnit::Bytes,
        ) else {
            return Err(TestError::Unexpected(
                "missing _bytes must be rejected".to_owned(),
            ));
        };
        assert!(err.contains("_bytes"));
        Ok(())
    }

    #[test]
    fn validate_metric_name_accepts_unit_substring_anywhere() {
        assert!(
            validate_metric_name(
                "harw_bytes_read_total",
                ParsedKind::Counter,
                ParsedUnit::Bytes
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_metric_name_count_and_ratio_need_no_suffix() {
        assert!(
            validate_metric_name("harw_context_cost", ParsedKind::Gauge, ParsedUnit::Count).is_ok()
        );
        assert!(
            validate_metric_name("harw_hit_rate", ParsedKind::Gauge, ParsedUnit::Ratio).is_ok()
        );
    }

    #[test]
    fn expand_metrics_rejects_duplicate_const_name() -> TestResult {
        let input: TokenStream = quote! {
            A: counter, unit = count, labels = [], cardinality = single, name = "harw_a_total";
            A: gauge, unit = count, labels = [], cardinality = single, name = "harw_b";
        };
        let Err(err) = expand_metrics(input) else {
            return Err(TestError::Unexpected(
                "duplicate const name must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("bereits doppelt vergeben"));
        Ok(())
    }

    #[test]
    fn expand_metrics_rejects_invalid_label() -> TestResult {
        let input: TokenStream = quote! {
            A: counter, unit = count, labels = ["Clan"], cardinality = single, name = "harw_a_total";
        };
        let Err(err) = expand_metrics(input) else {
            return Err(TestError::Unexpected(
                "invalid label must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("ungültiges Zeichen"));
        Ok(())
    }

    #[test]
    fn expand_metrics_rejects_unknown_key() -> TestResult {
        let input: TokenStream = quote! {
            A: counter, bogus = count, labels = [], cardinality = single, name = "harw_a_total";
        };
        let Err(err) = expand_metrics(input) else {
            return Err(TestError::Unexpected(
                "unknown key must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannter Schlüssel"));
        Ok(())
    }

    #[test]
    fn expand_metrics_accepts_valid_declaration() -> TestResult {
        let input: TokenStream = quote! {
            /// Wie viele Kinder aufgenommen wurden.
            CHILD_ADMITTED: counter, unit = count, labels = ["clan", "role"], cardinality = bounded(64),
                name = "harw_child_admitted_total";
            /// Aktuelle Kontextkosten.
            CONTEXT_COST: gauge, unit = tokens, labels = [], cardinality = single,
                name = "harw_context_cost_tokens";
        };
        let tokens = expand_metrics(input)
            .map_err(ctx("valid declaration must expand"))?
            .to_string();

        assert!(tokens.contains("pub const CHILD_ADMITTED"));
        assert!(tokens.contains("pub const CONTEXT_COST"));
        assert!(tokens.contains("MetricKind :: Counter"));
        assert!(tokens.contains("MetricKind :: Gauge"));
        assert!(tokens.contains("Unit :: Count"));
        assert!(tokens.contains("Unit :: Tokens"));
        assert!(tokens.contains("Cardinality :: Bounded"));
        assert!(tokens.contains("Cardinality :: Single"));
        assert!(tokens.contains("pub static ALL"));
        assert!(tokens.contains("& CHILD_ADMITTED"));
        assert!(tokens.contains("& CONTEXT_COST"));
        Ok(())
    }
}
