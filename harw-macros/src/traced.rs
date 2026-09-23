//! `#[traced]`-Expansion — instrumentierter `tracing`-Span für Funktionen.
//!
//! Spec: AW1-02-Brief (`#[traced]`). Baut auf dem Telemetrie-Vertrag aus
//! AW0-02 auf: Feldwerte laufen ausschließlich über `::harw_observe::Redact`
//! (siehe [`crate::redact`]), nie über `Debug`.
//!
//! # Grammatik
//!
//! ```ignore
//! #[harw_macros::traced(level = "debug", fields(child_id, clan))]
//! async fn admit_child(&self, child_id: &ChildId, clan: &ClanId) -> Result<(), Error> { … }
//! ```
//!
//! - `level = "..."` — einer von `trace`, `debug`, `info`, `warn`, `error`;
//!   optional, Voreinstellung `info`.
//! - `fields(a, b, ...)` — Namen von Parametern der annotierten Funktion
//!   (nicht `self`); optional, Voreinstellung: keine Felder.
//!
//! # Drei Entwurfsentscheidungen
//!
//! 1. **`async fn` → `.instrument(span)`, niemals `span.enter()`.**
//!    `span.enter()` liefert einen Guard, der über einen `.await`-Punkt hinweg
//!    gehalten werden kann. Pausiert der Task dort, bleibt der Span aktiv —
//!    läuft danach ein *anderer* Task auf demselben Executor-Thread weiter,
//!    erbt er denselben Span. Das Ergebnis sind Spans, die Arbeit enthalten,
//!    die nie in ihnen stattfand, und das ist an der Ausgabe selbst nicht als
//!    Fehler erkennbar — nur als falsche Zuordnung. Dieses Modul verschiebt
//!    den Körper einer `async fn` deshalb in ein inneres `async move { .. }`
//!    und führt es über `.instrument(span).await` aus:
//!    [`tracing::instrument::Instrument`] betritt den Span nur für die Dauer
//!    jedes einzelnen `poll()`-Aufrufs und verlässt ihn wieder, bevor der Task
//!    pausiert. Diese Umformung ändert nur den Funktionskörper, nie die
//!    Signatur — sie funktioniert deshalb unabhängig von Generics, Lifetimes
//!    oder dem Empfängertyp (`self`, `&self`, `&mut self`, kein Empfänger).
//! 2. **Feldwerte werden erst hinter der Aktivierungsprüfung berechnet.**
//!    Jedes Feld wird im Span zunächst mit `::tracing::field::Empty`
//!    deklariert; `Redact::redact(..)` läuft nur, wenn
//!    `!span.is_disabled()` gilt. Ein `fields(...)`-Argument, dessen
//!    `redact()` teuer ist, zahlt diese Kosten also nur, wenn der Level
//!    tatsächlich aktiv ist.
//! 3. **Argumente erscheinen über `Redact`, niemals über `Debug`.** Jedes
//!    `fields(...)`-Argument wird über `(<arg>).redact()` in ein `Redacted`
//!    überführt und darüber (via `::tracing::field::debug`) aufgezeichnet.
//!    Ein Argumenttyp ohne `Redact`-Implementierung erzeugt einen
//!    Compile-Fehler (Methode nicht gefunden) — es gibt keinen stillen
//!    Rückfall auf `{:?}` des Rohwerts.
//!
//! # `OperationMeta`-Anbindung — geprüft und verworfen
//!
//! `harw_operations::operation::OperationMeta` (siehe
//! `harw-operations/src/operation.rs`) hat die Felder `name`, `summary`,
//! `domain`, `permission`, `surfaces`, `aliases`, `category` und
//! `args_schema` — **kein** `level`-Feld und **keine** Liste zu
//! protokollierender Argumentnamen. Selbst wenn `#[traced]` zur Compile-Zeit
//! an eine `OperationMeta`-Instanz herankäme, gäbe es dort nichts, woraus
//! sich ein `tracing::Level` oder eine `fields(...)`-Liste ableiten ließe.
//! Erreichbar wäre allenfalls `name` als Span-Bezeichner — das dupliziert
//! aber nur den ohnehin vorhandenen Funktionsnamen, ohne neue Information zu
//! liefern.
//!
//! Unabhängig davon ist der Zugriff selbst nicht ohne Weiteres möglich: eine
//! `OperationMeta` lebt in einem privaten `OnceLock<OperationMeta>`, das
//! `<XOperation as Operation>::meta(&self)` je Operation zurückgibt — es gibt
//! in `harw-operations/src/operation.rs` keine Registrierung, die einen
//! Operationsnamen (`&str`) zur Compile-Zeit oder zur Laufzeit auf eine
//! `&'static OperationMeta` abbildet. Ein Proc-Macro sieht ohnehin nur den
//! Token-Strom seiner eigenen Funktion, keine Laufzeitwerte anderer Crates;
//! eine Laufzeit-Anbindung bräuchte eine solche Registrierungsfunktion, die
//! außerhalb des Schreibbereichs dieses Knotens läge (`harw-macros/**`) und
//! im gelesenen Quellumfang nicht existiert.
//!
//! Deshalb bleibt `#[traced]` bei der expliziten Form
//! `level = "..."`/`fields(...)`, ohne `operation = "..."`-Variante.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Block, FnArg, Ident, ItemFn, LitStr, Pat, Token};

/// Der geparste `level`-Wert aus `#[traced(level = "...")]`.
///
/// Unabhängig von `tracing::Level`: `harw-macros` hängt nicht produktiv von
/// `tracing` ab (siehe Modul-Doc von `lib.rs`); dieser Typ hält nur die für
/// Validierung und Codegen nötige Information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl ParsedLevel {
    /// Erzeugt den `::tracing::Level`-Pfad für diesen Level.
    fn to_tokens(self) -> TokenStream {
        match self {
            Self::Trace => quote! { ::tracing::Level::TRACE },
            Self::Debug => quote! { ::tracing::Level::DEBUG },
            Self::Info => quote! { ::tracing::Level::INFO },
            Self::Warn => quote! { ::tracing::Level::WARN },
            Self::Error => quote! { ::tracing::Level::ERROR },
        }
    }
}

/// Parst einen `level`-String (`trace`/`debug`/`info`/`warn`/`error`).
///
/// # Errors
/// Liefert `syn::Error`, wenn `lit` keiner der fünf erlaubten Werte ist.
///
/// # Design-doc reference
/// AW1-02-Brief (`#[traced]` — Attributgrammatik).
fn parse_level(lit: &LitStr) -> syn::Result<ParsedLevel> {
    match lit.value().as_str() {
        "trace" => Ok(ParsedLevel::Trace),
        "debug" => Ok(ParsedLevel::Debug),
        "info" => Ok(ParsedLevel::Info),
        "warn" => Ok(ParsedLevel::Warn),
        "error" => Ok(ParsedLevel::Error),
        other => Err(syn::Error::new_spanned(
            lit,
            format!("unbekannter level `{other}`; erwartet: trace, debug, info, warn, error"),
        )),
    }
}

/// Geparste `#[traced(...)]`-Argumente.
///
/// `level` fällt auf [`ParsedLevel::Info`] zurück, wenn nicht angegeben;
/// `fields` fällt auf eine leere Liste zurück.
#[derive(Debug)]
pub(crate) struct TracedArgs {
    level: ParsedLevel,
    fields: Vec<Ident>,
}

impl Parse for TracedArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut level: Option<ParsedLevel> = None;
        let mut fields: Option<Vec<Ident>> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "level" => {
                    if level.is_some() {
                        return Err(syn::Error::new_spanned(&key, "`level` doppelt angegeben"));
                    }
                    input.parse::<Token![=]>()?;
                    let lit: LitStr = input.parse()?;
                    level = Some(parse_level(&lit)?);
                }
                "fields" => {
                    if fields.is_some() {
                        return Err(syn::Error::new_spanned(&key, "`fields` doppelt angegeben"));
                    }
                    let content;
                    syn::parenthesized!(content in input);
                    let list = content.parse_terminated(Ident::parse, Token![,])?;
                    fields = Some(list.into_iter().collect());
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!("unbekannter Schlüssel `{other}`; erwartet: level, fields"),
                    ));
                }
            }

            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }

        Ok(TracedArgs {
            level: level.unwrap_or(ParsedLevel::Info),
            fields: fields.unwrap_or_default(),
        })
    }
}

/// Parst das `#[traced(...)]`-Attribut.
///
/// # Errors
/// - unbekannter Schlüssel (nicht `level`/`fields`) → `syn::Error`.
/// - `level` oder `fields` doppelt angegeben → `syn::Error`.
/// - ungültiger `level`-Wert → siehe [`parse_level`].
/// - Syntaxfehler (z. B. `fields` ohne Klammern) → `syn::Error` (von `syn`s
///   Parser).
///
/// # Design-doc reference
/// AW1-02-Brief (`#[traced]` — Attributgrammatik).
pub(crate) fn parse_traced_args(attr: TokenStream) -> syn::Result<TracedArgs> {
    syn::parse2(attr)
}

/// Sammelt die einfachen Bezeichner-Parameter (ohne `self`) einer
/// Funktionssignatur.
///
/// Ein Parameter mit destrukturierendem Muster (z. B. `(a, b): (u8, u8)`)
/// hat keinen einzelnen Namen und wird deshalb ausgelassen; er kann folglich
/// auch nicht in `fields(...)` referenziert werden.
fn collect_arg_idents(sig: &syn::Signature) -> Vec<Ident> {
    sig.inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => match pat_type.pat.as_ref() {
                Pat::Ident(pat_ident) => Some(pat_ident.ident.clone()),
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect()
}

/// Baut die Span-Erzeugung samt bedingter Feldaufzeichnung.
///
/// Erzeugt `let __harw_traced_span = ::tracing::span!(...)` mit allen
/// `fields`-Einträgen als `::tracing::field::Empty` deklariert, gefolgt von
/// einem `if !__harw_traced_span.is_disabled() { .. }`-Block, der jedes Feld
/// erst dort über `Redact::redact(..)` berechnet und via
/// `::tracing::field::debug(..)` aufzeichnet — siehe Modul-Doc, Punkt 2.
fn build_span_setup(span_name: &LitStr, level: TokenStream, fields: &[Ident]) -> TokenStream {
    let empty_field_decls: Vec<TokenStream> = fields
        .iter()
        .map(|ident| quote! { #ident = ::tracing::field::Empty })
        .collect();

    let span_invocation = if empty_field_decls.is_empty() {
        quote! { ::tracing::span!(#level, #span_name) }
    } else {
        quote! { ::tracing::span!(#level, #span_name, #(#empty_field_decls),*) }
    };

    let record_stmts: Vec<TokenStream> = fields
        .iter()
        .map(|ident| {
            let name_lit = LitStr::new(&ident.to_string(), ident.span());
            quote! {
                {
                    use ::harw_observe::Redact as _;
                    let __harw_traced_value = (#ident).redact();
                    __harw_traced_span.record(
                        #name_lit,
                        &::tracing::field::debug(&__harw_traced_value),
                    );
                }
            }
        })
        .collect();

    quote! {
        let __harw_traced_span = #span_invocation;
        if !__harw_traced_span.is_disabled() {
            #(#record_stmts)*
        }
    }
}

/// Expander für das `#[traced]`-Attribut-Makro.
///
/// Validiert, dass jeder `fields(...)`-Eintrag einen tatsächlichen Parameter
/// der annotierten Funktion benennt, und ersetzt dann nur den Funktionskörper
/// (`func.block`) — Sichtbarkeit, Generics, Empfänger und Signatur bleiben
/// unverändert. Für `async fn` entsteht `.instrument(span).await` (nie
/// `span.enter()`); für `fn` entsteht `span.enter()`. Siehe Modul-Doc, Punkt 1.
///
/// # Errors
/// - ein `fields(...)`-Eintrag benennt keinen Parameter der Funktion →
///   `syn::Error`.
///
/// # Design-doc reference
/// AW1-02-Brief (`#[traced]`).
pub(crate) fn expand_traced(func: ItemFn, args: TracedArgs) -> syn::Result<TokenStream> {
    let arg_idents = collect_arg_idents(&func.sig);

    for field_ident in &args.fields {
        if !arg_idents.iter().any(|a| a == field_ident) {
            return Err(syn::Error::new_spanned(
                field_ident,
                format!(
                    "`{field_ident}` ist kein Argument dieser Funktion; \
                     #[traced(fields(...))] darf nur vorhandene Parameternamen referenzieren"
                ),
            ));
        }
    }

    let span_name = LitStr::new(&func.sig.ident.to_string(), func.sig.ident.span());
    let level_tokens = args.level.to_tokens();
    let span_setup = build_span_setup(&span_name, level_tokens, &args.fields);

    let original_block = &func.block;
    let is_async = func.sig.asyncness.is_some();

    let new_block: Block = if is_async {
        syn::parse2(quote! {{
            #span_setup
            {
                use ::tracing::Instrument as _;
                async move #original_block.instrument(__harw_traced_span).await
            }
        }})?
    } else {
        syn::parse2(quote! {{
            #span_setup
            let _harw_traced_guard = __harw_traced_span.enter();
            #original_block
        }})?
    };

    let mut new_func = func;
    // In die bestehende Box schreiben statt eine neue anzulegen -- die alte
    // würde sonst nur zum Wegwerfen alloziert.
    *new_func.block = new_block;

    Ok(quote! { #new_func })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn parse_args(tokens: TokenStream) -> syn::Result<TracedArgs> {
        parse_traced_args(tokens)
    }

    #[test]
    fn parse_traced_args_defaults_to_info_and_no_fields() -> TestResult {
        let args = parse_args(quote! {}).map_err(ctx("empty attribute must parse"))?;
        assert_eq!(args.level, ParsedLevel::Info);
        assert!(args.fields.is_empty());
        Ok(())
    }

    #[test]
    fn parse_traced_args_reads_level_and_fields() -> TestResult {
        let args = parse_args(quote! { level = "debug", fields(child_id, clan) })
            .map_err(ctx("valid attribute must parse"))?;
        assert_eq!(args.level, ParsedLevel::Debug);
        assert_eq!(args.fields.len(), 2);
        assert_eq!(args.fields[0], "child_id");
        assert_eq!(args.fields[1], "clan");
        Ok(())
    }

    #[test]
    fn parse_traced_args_accepts_fields_only() -> TestResult {
        let args =
            parse_args(quote! { fields(a) }).map_err(ctx("fields-only attribute must parse"))?;
        assert_eq!(args.level, ParsedLevel::Info);
        assert_eq!(args.fields.len(), 1);
        Ok(())
    }

    #[test]
    fn parse_traced_args_rejects_unknown_key() -> TestResult {
        let Err(err) = parse_args(quote! { bogus = "x" }) else {
            return Err(TestError::Unexpected(
                "unknown key must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannter Schlüssel"));
        Ok(())
    }

    #[test]
    fn parse_traced_args_rejects_invalid_level() -> TestResult {
        let Err(err) = parse_args(quote! { level = "verbose" }) else {
            return Err(TestError::Unexpected(
                "invalid level must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannter level"));
        Ok(())
    }

    #[test]
    fn parse_traced_args_rejects_duplicate_level() -> TestResult {
        let Err(err) = parse_args(quote! { level = "info", level = "debug" }) else {
            return Err(TestError::Unexpected(
                "duplicate level must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("doppelt angegeben"));
        Ok(())
    }

    #[test]
    fn parse_traced_args_rejects_duplicate_fields() -> TestResult {
        let Err(err) = parse_args(quote! { fields(a), fields(b) }) else {
            return Err(TestError::Unexpected(
                "duplicate fields must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("doppelt angegeben"));
        Ok(())
    }

    #[test]
    fn expand_traced_rejects_field_not_an_argument() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn foo(present: i32) -> i32 { present }
        };
        let args = TracedArgs {
            level: ParsedLevel::Info,
            fields: vec![syn::parse_quote!(missing)],
        };
        let Err(err) = expand_traced(func, args) else {
            return Err(TestError::Unexpected(
                "unknown field name must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("kein Argument"));
        Ok(())
    }

    #[test]
    fn expand_traced_sync_fn_uses_enter_not_instrument() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn compute(x: i32) -> i32 { x * 2 }
        };
        let args = TracedArgs {
            level: ParsedLevel::Info,
            fields: vec![],
        };
        let tokens = expand_traced(func, args)
            .map_err(ctx("sync fn must expand"))?
            .to_string();
        assert!(tokens.contains("enter"));
        assert!(!tokens.contains("instrument"));
        assert!(tokens.contains("is_disabled"));
        Ok(())
    }

    #[test]
    fn expand_traced_async_fn_uses_instrument_not_enter() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn compute(x: i32) -> i32 { x * 2 }
        };
        let args = TracedArgs {
            level: ParsedLevel::Info,
            fields: vec![],
        };
        let tokens = expand_traced(func, args)
            .map_err(ctx("async fn must expand"))?
            .to_string();
        assert!(tokens.contains("instrument"));
        assert!(!tokens.contains("enter"));
        assert!(tokens.contains("async move"));
        Ok(())
    }

    #[test]
    fn expand_traced_emits_redact_call_for_each_field() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn admit(child_id: &ChildId, clan: &ClanId) {}
        };
        let args = TracedArgs {
            level: ParsedLevel::Debug,
            fields: vec![syn::parse_quote!(child_id), syn::parse_quote!(clan)],
        };
        let tokens = expand_traced(func, args)
            .map_err(ctx("fn with fields must expand"))?
            .to_string();
        assert!(tokens.contains(":: harw_observe :: Redact"));
        assert!(tokens.contains("redact"));
        assert!(tokens.contains(":: tracing :: field :: debug"));
        assert!(tokens.contains(":: tracing :: field :: Empty"));
        assert!(tokens.contains("Level :: DEBUG"));
        assert!(tokens.contains("\"child_id\""));
        assert!(tokens.contains("\"clan\""));
        Ok(())
    }

    #[test]
    fn expand_traced_without_fields_emits_no_redact_call() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn noop() {}
        };
        let args = TracedArgs {
            level: ParsedLevel::Info,
            fields: vec![],
        };
        let tokens = expand_traced(func, args)
            .map_err(ctx("fn without fields must expand"))?
            .to_string();
        assert!(!tokens.contains("Redact"));
        assert!(tokens.contains("Level :: INFO"));
        Ok(())
    }

    #[test]
    fn parsed_level_to_tokens_maps_all_variants() {
        assert!(ParsedLevel::Trace.to_tokens().to_string().contains("TRACE"));
        assert!(ParsedLevel::Warn.to_tokens().to_string().contains("WARN"));
        assert!(ParsedLevel::Error.to_tokens().to_string().contains("ERROR"));
    }
}
