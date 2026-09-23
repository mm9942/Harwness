//! `#[context_provider]`- und `#[instructions_provider]`-Expansion.
//!
//! Ersetzt das wiederholte `Box::pin(async move { … })`-Boilerplate, das jede
//! Implementierung von `harw_extension_api::ContextProvider` bzw.
//! `harw_extension_api::InstructionsProvider` bisher von Hand schreiben musste
//! (siehe Vorbild `harw-extension-api/src/contributors.rs` sowie die
//! Implementierungen in `harw-project-discovery/src/provider.rs` und
//! `harw-instructions/src/baseline.rs`).
//!
//! Beide Makros werden auf eine **freie `async fn`** angewendet und erzeugen:
//!
//! 1. Eine (Unit- oder Zustands-)Struct, benannt nach der Funktion
//!    (`PascalCase(fn_name) + "Provider"`, überschreibbar via
//!    `struct_name = "..."`). Die `#[doc]`-Attribute der annotierten Funktion
//!    werden unverändert auf diese Struct übertragen: die Funktion selbst
//!    bleibt privat (kein `pub`) und wird deshalb standardmäßig nicht in
//!    rustdoc gerendert — ohne diese Übertragung würde jede Migration eines
//!    handschriftlichen Providers auf dieses Makro seine öffentliche Doku
//!    stillschweigend löschen. Der zustandslose Zweig leitet zusätzlich
//!    `Debug, Clone, Copy, Default` ab; der zustandsbehaftete Zweig leitet nur
//!    `Debug` ab, weil der `state`-Feldtyp beliebig ist — weder `Copy` noch
//!    `Default` lassen sich für einen beliebigen Zustandstyp garantieren, und
//!    ein `Clone`, das bei fehlender `Clone`-Implementierung des Zustands mit
//!    einem entfernten Compile-Fehler überrascht, wäre schlechter als gar kein
//!    Derive. `Debug` ist dagegen für praktisch jeden sinnvollen Zustandstyp
//!    (Konfiguration, gesammelter Kontext) erwartbar und für Tracing/Logging
//!    nützlich.
//! 2. Einen `new(...)`-Konstruktor mit eigener, generischer Doku (die Struct
//!    selbst trägt bereits die spezifische Doku aus Punkt 1).
//! 3. Eine `impl`-Block für das jeweilige Contributor-Trait, dessen
//!    dokumentierte Methode die annotierte Funktion in `Box::pin(...)`
//!    einbettet.
//!
//! `parse_context_provider_args`, `expand_context_provider`,
//! `parse_instructions_provider_args` und `expand_instructions_provider` sind
//! die Einstiegspunkte, die die `#[proc_macro_attribute]`-Funktionen
//! `context_provider` bzw. `instructions_provider` im Crate-Root (`lib.rs`)
//! aufrufen — Proc-Macro-Crates dürfen `#[proc_macro*]`-Funktionen nur dort
//! exportieren.
//!
//! # Namensraum, Vertrauensklasse und Kosten (`#[context_provider]`, AW3-03)
//!
//! Befund des Knotens AW3-03: `#[context_provider]` konnte weder einen
//! Namensraum noch eine Vertrauensklasse deklarieren, und
//! `ExtensionRegistryBuilder::context_provider` prüfte beides nicht bei der
//! Registrierung — ein Fragmentanbieter durfte seine eigene `TrustClass`
//! frei behaupten, und zwei Anbieter konnten denselben Namensraum
//! beanspruchen, ohne dass das auffiel. AW3-03 hat die Prüfung selbst schon
//! einmal gebaut, aber notgedrungen in `harw-plan-bridge::fragment_registry`
//! (außerhalb des Schreibbereichs dieses Knotens) — eine Prüfung, die nur
//! greift, wenn ein Anbieter freiwillig durch diese Crate geht. Dieser
//! Abschnitt beschreibt, was `#[context_provider]` jetzt zusätzlich
//! deklariert; die Durchsetzung selbst steht in
//! `harw_extension_api::registry` (siehe dortige Moduldoku).
//!
//! Zwei neue Attributschlüssel, exakt zwei — nicht drei, siehe unten zu
//! `cost`:
//!
//! - **`namespace = "..."`** (Zeichenkette, optional). Sektionspräfix, unter
//!   dem dieser Provider liefert; Grundlage der Kollisionsprüfung bei der
//!   Registrierung. Zulässige Zeichen sind ausschließlich `[a-z0-9_]` —
//!   **bewusst ohne Punkt**, im Unterschied zu
//!   [`crate::field::validate_field_name`] (das für `harw-observe`-Feldnamen
//!   `.` als Hierarchietrenner erlaubt). Ein Namensraum ist kein
//!   hierarchischer Pfad, sondern ein flacher Kollisionsschlüssel; ein Punkt
//!   oder ein Stern darin würde später mit Glob- oder Sektions-Routing-Regeln
//!   kollidieren (siehe `harw_dod_readfs::glob::glob`, `SectionName`). Die
//!   Regel entspricht stattdessen
//!   `harw_observe::MetricKey`-Namen (`crate::metrics::validate_metric_name`,
//!   dieselbe Zeichenklasse). Ein leerer Namensraum ist ein Compile-Fehler —
//!   sowohl explizit angegeben als auch, falls der abgeleitete Vorgabewert
//!   (siehe unten) aus irgendeinem Grund leer wäre.
//!
//!   **Optional, mit Vorgabewert `fn_name`** (dieselbe Quelle wie `NAME`s
//!   Vorgabewert). Verpflichtend zu machen hieße: der einzige heutige
//!   Konsument (`harw-project-discovery::provider::project_context`, siehe
//!   Abschlussbericht dieses Knotens) bricht sofort und liegt außerhalb des
//!   Schreibbereichs dieses Knotens — nicht behebbar von hier aus. Der
//!   Funktionsname ist immer ein gültiger, nicht-leerer Rust-Bezeichner und
//!   damit ein sicherer, kollisionsarmer Vorgabewert; er wird trotzdem durch
//!   dieselbe Zeichenprüfung geschickt wie ein expliziter Wert, damit ein
//!   Bezeichner außerhalb der Konvention (Großbuchstaben) einen klaren
//!   Compile-Fehler erzeugt statt eines still kaputten Namensraums.
//!
//! - **`trust = Instruction | Evidence | Data`** (Bezeichner, kein
//!   String-Literal — dieselbe Technik wie `#[sensor(capability = ...)]` in
//!   [`crate::sensor_source`]: [`validate_trust_class`] prüft den Bezeichner
//!   gegen [`TRUST_CLASS_VARIANTS`], eine bewusst gepflegte Textkopie der
//!   drei `harw_context::TrustClass`-Variantennamen — `harw-macros` hängt
//!   nicht produktiv von `harw-context` ab, der erzeugte Pfad
//!   `::harw_extension_api::TrustClass::#trust` ist rein textuell). **Optional**;
//!   Vorgabewert ist `TrustClass::Data`, die *niedrigste* Klasse.
//!
//!   Das ist eine Sicherheitsentscheidung, keine Bequemlichkeit: `Data` ist
//!   die einzige Klasse, die niemals im Instruktionsblock landen kann (siehe
//!   `harw_context::TrustClass::trust_rank` und
//!   `harw-extension-api/src/v1_compat.rs`, das aus demselben Grund
//!   ausnahmslos `TrustClass::Data` für v1-Fragmente setzt). Ein Provider,
//!   der nichts deklariert, bekommt also die Behandlung „bis zum Beweis des
//!   Gegenteils nicht besonders vertrauenswürdig" — nie „unbekannt behandelt
//!   wie vertrauenswürdig", und nie die höchste Klasse. Optional statt
//!   verpflichtend aus demselben Rückwirkungsgrund wie bei `namespace`: der
//!   eine heutige Konsument bricht sonst ohne Reparaturmöglichkeit von hier
//!   aus.
//!
//! - **`cost` existiert absichtlich NICHT als Attributschlüssel.** Ein
//!   `harw_lens_types::CostEstimator` ist ein Trait-Objekt
//!   (`Arc<dyn CostEstimator>`), kein Attributliteral — ein Pfad-Attribut
//!   (`cost = BytesOverFour`, nach demselben Muster wie `trust`) wäre zwar
//!   technisch machbar, aber `BytesOverFour` ist laut
//!   `harw-plan-bridge/src/fragment_registry.rs` „der einzige Kostenschätzer
//!   dieser Crate-Landschaft" — jeder Provider würde also exakt denselben
//!   Wert wiederholen. Schwerer wiegt: `harw_extension_api::ContextFragment`
//!   (die Struct, die die annotierte Funktion tatsächlich zurückgibt) hat
//!   heute **kein** Kostenfeld — ein deklarierter Kostenschätzer hätte also
//!   nirgends einen Abnehmer. Ein Attributschlüssel, der immer denselben Wert
//!   trägt und den nichts ausliest, ist genau das wirkungslose Attribut, vor
//!   dem der Auftrag warnt (`#[from]` auf einem benannten Feld war dasselbe
//!   Muster: syntaktisch gültig, semantisch folgenlos, drei Agenten Zeit
//!   gekostet). Sollte `ContextFragment` künftig ein Kostenfeld bekommen und
//!   mehr als ein `CostEstimator` in dieser Crate-Landschaft existieren, ist
//!   das der Zeitpunkt, an dem `cost` als dritter Schlüssel Sinn ergibt —
//!   heute nicht.
//!
//! `#[instructions_provider]` kennt weder `namespace` noch `trust`: es
//! erzeugt `LoadedInstructions { system_prompt: String, fragments: Vec<String> }`
//! — reinen Text ohne Fragment-/Trust-Konzept —, und der Befund benennt
//! ausdrücklich nur `expand_context_provider`. [`ContextProviderArgs`] ist
//! deshalb ein eigener, von [`ContributorArgs`] getrennter Argument-Typ:
//! `#[instructions_provider(namespace = "...")]` bleibt ein unbekannter
//! Schlüssel und damit ein Compile-Fehler, statt ein Schlüssel zu sein, der
//! dort klaglos geparst würde, aber nichts bewirkt.
//!
//! ## Wie `NAMESPACE`/`TRUST` bei der Registrierung ankommen
//!
//! `expand_context_provider` erzeugt zusätzlich zu `NAMESPACE`/`TRUST` zwei
//! Dinge, die beide dieselben Werte tragen, aber über verschiedene Wege
//! ankommen:
//!
//! 1. `impl harw_extension_api::registry::DeclaredContextProvider for
//!    #struct_ident` — die ursprüngliche, separate Deklarationsspur aus dem
//!    Vorgänger-Nachzug. Sie bleibt bestehen (siehe Redundanz-Hinweis unten).
//! 2. **Nachzug (dieser Knoten):** `impl
//!    harw_extension_api::contributors::ContextProvider for #struct_ident`
//!    überschreibt jetzt zusätzlich zu `contribute` auch `namespace()` und
//!    `max_trust()` — beide geben `Self::NAMESPACE`/`Self::TRUST` zurück statt
//!    der Trait-Vorgabewerte (`std::any::type_name::<Self>()` bzw.
//!    `TrustClass::Data`). Diese beiden Methoden sitzen bewusst direkt auf
//!    `ContextProvider` selbst (siehe
//!    `harw_extension_api::contributors`-Moduldoku): eine virtuelle Methode
//!    überlebt `Arc::new(...) as Arc<dyn ContextProvider>`, eine assoziierte
//!    Konstante nicht. Vor diesem Nachzug sah der ungeprüfte, aber nach wie
//!    vor existierende `ExtensionRegistryBuilder::context_provider(Arc<dyn
//!    ContextProvider>)`-Pfad deshalb nur die Vorgabewerte eines
//!    makro-erzeugten Providers, während
//!    `ExtensionRegistryBuilder::context_provider_declared` bereits die
//!    echten deklarierten Werte sah — beide Wege prüfen seit dem
//!    Trait-Knoten, aber nur einer prüfte die richtigen Werte. Mit diesem
//!    Nachzug sehen beide Wege dieselben, tatsächlich deklarierten Werte.
//!
//! **Redundanzfrage:** Mit dieser Änderung wird
//! `impl DeclaredContextProvider` für jeden makro-erzeugten Typ streng
//! redundant zu `impl ContextProvider::namespace/max_trust` — beide liefern
//! exakt dieselben Werte über zwei Methoden mit unterschiedlichem Namen.
//! Diese Datei entfernt `DeclaredContextProvider` trotzdem **nicht**: der
//! Trait selbst lebt in `harw_extension_api::registry`, und ein Entfernen
//! der Erzeugung hier, ohne den Trait bzw.
//! `ExtensionRegistryBuilder::context_provider_declared` dort ebenfalls
//! anzufassen, hinterließe einen Trait ohne generierte Implementierungen.
//! Das ist eine bewusste Entscheidung für einen späteren, eigenen Knoten in
//! `harw-extension-api`, nicht ein offen gelassenes Detail.
//!
//! # Design-Doc-Referenz
//! AP W1-26f, Teil 1 (`harw-macros/src/contributor.rs`); AW3-03-Befund
//! (Namensraum/Trust/Kosten-Ergänzung von `#[context_provider]`).

use crate::util::pascal_case;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{FnArg, Ident, ItemFn, LitStr, Type, spanned::Spanned};

// ── Attribut-Argumente ───────────────────────────────────────────────────────

/// Geparste `#[instructions_provider(...)]`-Argumente.
///
/// Nur noch von `#[instructions_provider]` verwendet — `#[context_provider]`
/// hat seit AW3-03 einen eigenen, reicheren Argument-Typ, [`ContextProviderArgs`]
/// (siehe Moduldoku, Abschnitt „Namensraum, Vertrauensklasse und Kosten").
/// Zwei optionale Schlüssel:
/// - `name`: rein deklarativ, nur für Doku/Tracing (landet als `NAME`-Assoziierte-Konstante
///   auf der generierten Struct); Default ist der Funktionsname.
/// - `struct_name`: überschreibt die aus dem Funktionsnamen abgeleitete
///   `PascalCase(fn_name) + "Provider"`-Konvention.
#[derive(Debug, Default)]
pub(crate) struct ContributorArgs {
    name: Option<LitStr>,
    struct_name: Option<LitStr>,
}

/// Parst den rohen Attribut-Token-Stream für `#[instructions_provider(...)]`
/// in [`ContributorArgs`].
///
/// `macro_name` wird nur für die Fehlermeldungen verwendet.
///
/// # Errors
/// `syn::Error` bei unbekanntem oder doppelt angegebenem Attribut-Schlüssel.
fn parse_contributor_args(
    attr: proc_macro2::TokenStream,
    macro_name: &str,
) -> syn::Result<ContributorArgs> {
    let mut name: Option<LitStr> = None;
    let mut struct_name: Option<LitStr> = None;

    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") {
            if name.is_some() {
                return Err(meta.error(format!("duplicate `{macro_name}` attribute field `name`")));
            }
            name = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("struct_name") {
            if struct_name.is_some() {
                return Err(meta.error(format!(
                    "duplicate `{macro_name}` attribute field `struct_name`"
                )));
            }
            struct_name = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error(format!(
                "unknown `{macro_name}` attribute field; expected `name` or `struct_name`"
            )))
        }
    });
    parser.parse2(attr)?;

    Ok(ContributorArgs { name, struct_name })
}

/// Parst `#[instructions_provider(...)]`-Argumente.
///
/// # Errors
/// Siehe [`parse_contributor_args`].
pub(crate) fn parse_instructions_provider_args(
    attr: proc_macro2::TokenStream,
) -> syn::Result<ContributorArgs> {
    parse_contributor_args(attr, "instructions_provider")
}

// ── `#[context_provider]`-Argumente: `name`, `struct_name`, `namespace`, `trust` ──

/// Die drei erlaubten `harw_context::TrustClass`-Variantennamen, in
/// Deklarationsreihenfolge.
///
/// `harw-macros` hängt nicht produktiv von `harw-context` ab (siehe
/// Moduldoku von `lib.rs`) — diese Liste ist deshalb eine bewusst gepflegte
/// Textkopie, keine Typprüfung, exakt nach dem Vorbild von
/// [`crate::sensor_source::CAPABILITY_VARIANTS`]. Eine vierte `TrustClass`-
/// Variante ist unwahrscheinlich (die Moduldoku von
/// `harw-context/src/fragment.rs` nennt die Klasse ausdrücklich
/// „geschlossen, drei Werte"), erforderte aber ohnehin eine bewusste,
/// sichtbare Ergänzung hier.
const TRUST_CLASS_VARIANTS: &[&str] = &["Instruction", "Evidence", "Data"];

/// Prüft, ob `ident` einer der drei erlaubten `TrustClass`-Variantennamen ist.
///
/// # Errors
/// `syn::Error`, gespannt auf `ident`, wenn der Name in
/// [`TRUST_CLASS_VARIANTS`] nicht vorkommt.
fn validate_trust_class(ident: &Ident) -> syn::Result<()> {
    let name = ident.to_string();
    if TRUST_CLASS_VARIANTS.contains(&name.as_str()) {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            ident,
            format!(
                "unknown trust class `{name}`; expected one of: {}",
                TRUST_CLASS_VARIANTS.join(", ")
            ),
        ))
    }
}

/// Prüft, ob `name` ein gültiger `#[context_provider(namespace = ...)]`-Wert ist.
///
/// Nicht leer, ausschließlich Zeichen aus `[a-z0-9_]` — dieselbe Zeichenklasse
/// wie `harw_observe::MetricKey`-Namen (siehe Moduldoku, Abschnitt
/// „Namensraum, Vertrauensklasse und Kosten", für die Begründung, warum ein
/// Punkt hier — anders als bei `harw_observe`-Feldnamen — nicht erlaubt ist).
///
/// # Errors
/// Eine menschenlesbare Beschreibung, wenn `name` leer ist oder ein Zeichen
/// außerhalb `[a-z0-9_]` enthält.
fn validate_namespace(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("context_provider namespace must not be empty".to_owned());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '_'))
    {
        return Err(format!(
            "context_provider namespace `{name}` contains invalid character '{c}'; \
             allowed characters are [a-z0-9_]"
        ));
    }
    Ok(())
}

/// Geparste `#[context_provider(...)]`-Argumente.
///
/// Eigener Argument-Typ statt [`ContributorArgs`] (siehe Moduldoku): neben
/// `name` und `struct_name` kommen zwei Context-Provider-spezifische
/// Schlüssel hinzu, `namespace` und `trust` — beide optional, beide mit einem
/// sicheren Vorgabewert (siehe Moduldoku für die Begründung).
#[derive(Debug, Default)]
pub(crate) struct ContextProviderArgs {
    name: Option<LitStr>,
    struct_name: Option<LitStr>,
    namespace: Option<LitStr>,
    trust: Option<Ident>,
}

/// Parst `#[context_provider(...)]`-Argumente in [`ContextProviderArgs`].
///
/// Folgt derselben `syn::meta::parser`-Form wie [`parse_contributor_args`],
/// erweitert um `namespace` (Zeichenkette, sofort gegen [`validate_namespace`]
/// geprüft) und `trust` (Bezeichner, sofort gegen [`validate_trust_class`]
/// geprüft — dieselbe Technik wie `#[sensor(capability = ...)]`, siehe
/// [`crate::sensor_source::parse_sensor_args`]).
///
/// # Errors
/// - unbekanntes oder doppelt angegebenes Attribut-Feld;
/// - `namespace` ist leer oder enthält ein Zeichen außerhalb `[a-z0-9_]`;
/// - `trust` benennt keine der drei `TrustClass`-Varianten.
pub(crate) fn parse_context_provider_args(
    attr: proc_macro2::TokenStream,
) -> syn::Result<ContextProviderArgs> {
    let mut name: Option<LitStr> = None;
    let mut struct_name: Option<LitStr> = None;
    let mut namespace: Option<LitStr> = None;
    let mut trust: Option<Ident> = None;

    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") {
            if name.is_some() {
                return Err(meta.error("duplicate `context_provider` attribute field `name`"));
            }
            name = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("struct_name") {
            if struct_name.is_some() {
                return Err(
                    meta.error("duplicate `context_provider` attribute field `struct_name`")
                );
            }
            struct_name = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("namespace") {
            if namespace.is_some() {
                return Err(meta.error("duplicate `context_provider` attribute field `namespace`"));
            }
            let lit: LitStr = meta.value()?.parse()?;
            validate_namespace(&lit.value())
                .map_err(|reason| syn::Error::new_spanned(&lit, reason))?;
            namespace = Some(lit);
            Ok(())
        } else if meta.path.is_ident("trust") {
            if trust.is_some() {
                return Err(meta.error("duplicate `context_provider` attribute field `trust`"));
            }
            let ident: Ident = meta.value()?.parse()?;
            validate_trust_class(&ident)?;
            trust = Some(ident);
            Ok(())
        } else {
            Err(meta.error(
                "unknown `context_provider` attribute field; expected `name`, `struct_name`, \
                 `namespace`, or `trust`",
            ))
        }
    });
    parser.parse2(attr)?;

    Ok(ContextProviderArgs {
        name,
        struct_name,
        namespace,
        trust,
    })
}

// ── Gemeinsame Signatur-Helfer ───────────────────────────────────────────────

/// Prüft, ob ein Funktionsargument ein Referenztyp ist (`&T` oder `&mut T`).
fn is_reference_arg(arg: &FnArg) -> bool {
    matches!(arg, FnArg::Typed(pat) if matches!(pat.ty.as_ref(), Type::Reference(_)))
}

/// Extrahiert den referenzierten Zieltyp `T` aus einem `&T`-Argument.
///
/// Gibt `None` zurück, wenn das Argument kein Referenztyp ist (oder ein
/// `self`-Empfänger ist, der hier nie vorkommen darf, da es sich um freie
/// Funktionen handelt).
fn reference_inner_type(arg: &FnArg) -> Option<Type> {
    match arg {
        FnArg::Typed(pat) => match pat.ty.as_ref() {
            Type::Reference(r) => Some((*r.elem).clone()),
            _ => None,
        },
        FnArg::Receiver(_) => None,
    }
}

/// Löst den Struct-Bezeichner auf: `struct_name`-Override oder
/// `PascalCase(fn_name) + "Provider"`.
///
/// Nimmt `struct_name` als eigenständiges `Option<&LitStr>` statt eines
/// ganzen Argument-Structs entgegen, damit sowohl [`ContributorArgs`] als
/// auch [`ContextProviderArgs`] (zwei unterschiedliche Typen, siehe
/// Moduldoku) dieselbe Auflösung teilen können.
///
/// # Errors
/// `syn::Error`, wenn `struct_name` kein gültiger Rust-Bezeichner ist.
fn resolve_struct_ident(struct_name: Option<&LitStr>, fn_name: &str) -> syn::Result<syn::Ident> {
    match struct_name {
        Some(lit) => syn::parse_str::<syn::Ident>(&lit.value()).map_err(|_| {
            syn::Error::new_spanned(lit, "`struct_name` must be a valid Rust identifier")
        }),
        None => Ok(format_ident!("{}Provider", pascal_case(fn_name))),
    }
}

// ── `#[context_provider]` ────────────────────────────────────────────────────

/// Verwandelt eine `async fn(ctx: &TurnInputContext[, state: &StateType]) -> Vec<ContextFragment>`
/// in eine (Unit- oder Zustands-)Struct plus `impl harw_extension_api::ContextProvider`.
///
/// # Errors
/// - nicht-`async fn`
/// - falsche Parameteranzahl (weder 1 noch 2)
/// - erster Parameter (`ctx`) ist kein Referenztyp
/// - zweiter Parameter (`state`), falls vorhanden, ist kein Referenztyp
///
/// # Design-Doc-Referenz
/// AP W1-26f, Teil 1, Makro 1.
pub(crate) fn expand_context_provider(
    func: ItemFn,
    args: ContextProviderArgs,
) -> syn::Result<proc_macro2::TokenStream> {
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[context_provider] requires an `async fn`",
        ));
    }

    let mut inputs = func.sig.inputs.iter();
    let ctx_arg = inputs.next().ok_or_else(|| {
        syn::Error::new(
            func.sig.span(),
            "#[context_provider] requires at least `(ctx: &TurnInputContext)`",
        )
    })?;
    if !is_reference_arg(ctx_arg) {
        return Err(syn::Error::new_spanned(
            ctx_arg,
            "the first `#[context_provider]` argument must be a reference \
             (e.g. `&TurnInputContext`)",
        ));
    }

    let state_arg = inputs.next();
    if inputs.next().is_some() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[context_provider] accepts at most `(ctx: &TurnInputContext, state: &StateType)`",
        ));
    }

    let state_type = match state_arg {
        Some(arg) => Some(reference_inner_type(arg).ok_or_else(|| {
            syn::Error::new_spanned(
                arg,
                "the second `#[context_provider]` argument (state) must be a reference \
                 (e.g. `&StateType`)",
            )
        })?),
        None => None,
    };

    let fn_ident = func.sig.ident.clone();
    let fn_name_str = fn_ident.to_string();
    let struct_ident = resolve_struct_ident(args.struct_name.as_ref(), &fn_name_str)?;
    let name_str = args
        .name
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| fn_name_str.clone());

    // `namespace`: expliziter Wert wurde bereits in `parse_context_provider_args`
    // gegen `validate_namespace` geprüft. Fehlt er, ist der Funktionsname der
    // Vorgabewert (dieselbe Quelle wie `NAME`s Vorgabewert) — der aber, weil er
    // nie durch `validate_namespace` gelaufen ist, hier nachträglich geprüft
    // werden muss: ein Funktionsname mit Großbuchstaben wäre sonst ein still
    // kaputter Namensraum statt eines klaren Compile-Fehlers (siehe Moduldoku).
    let namespace_str = match &args.namespace {
        Some(lit) => lit.value(),
        None => {
            validate_namespace(&fn_name_str).map_err(|reason| {
                syn::Error::new(
                    fn_ident.span(),
                    format!(
                        "#[context_provider] has no explicit `namespace`, and the function \
                         name is not a valid default namespace: {reason}"
                    ),
                )
            })?;
            fn_name_str.clone()
        }
    };

    // `trust`: Vorgabewert ist `TrustClass::Data`, die niedrigste Klasse
    // (Sicherheitsentscheidung, siehe Moduldoku).
    let trust_ident: Ident = args.trust.clone().unwrap_or_else(|| format_ident!("Data"));

    // Die annotierte Funktion bleibt privat (kein `pub`) und wird deshalb nie
    // in öffentlichem rustdoc gerendert. Ihre `#[doc]`-Attribute (die
    // desugarten `///`-Kommentare) werden deshalb unverändert auf die
    // generierte Struct übertragen — siehe Moduldoc, Punkt 1.
    let doc_attrs: Vec<&syn::Attribute> = func
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .collect();

    let (struct_def, ctor, call_expr) = match &state_type {
        Some(ty) => (
            quote! {
                #(#doc_attrs)*
                #[derive(Debug)]
                pub struct #struct_ident {
                    state: #ty,
                }
            },
            quote! {
                /// Erstellt eine neue Instanz, die den übergebenen `state`
                /// kapselt.
                ///
                /// # Arguments
                /// - `state`: wird bei jedem Aufruf unverändert per Referenz
                ///   an die mit `#[context_provider]` annotierte Funktion
                ///   weitergereicht.
                ///
                /// # Returns
                /// Eine registrierbare `ContextProvider`-Instanz.
                pub fn new(state: #ty) -> Self {
                    Self { state }
                }
            },
            quote! { #fn_ident(ctx, &self.state) },
        ),
        None => (
            quote! {
                #(#doc_attrs)*
                #[derive(Debug, Clone, Copy, Default)]
                pub struct #struct_ident;
            },
            quote! {
                /// Erstellt eine neue Instanz dieses zustandslosen Providers.
                ///
                /// # Returns
                /// Eine registrierbare `ContextProvider`-Instanz.
                pub fn new() -> Self {
                    Self
                }
            },
            quote! { #fn_ident(ctx) },
        ),
    };

    Ok(quote! {
        #func

        #struct_def

        impl #struct_ident {
            /// Maschinenlesbarer Contributor-Name (nur Doku/Tracing) — siehe
            /// `#[context_provider(name = "...")]`. Default ist der Funktionsname.
            pub const NAME: &'static str = #name_str;

            /// Sektionspräfix, unter dem dieser Provider bei der
            /// Registry-Prüfung antritt — siehe
            /// `#[context_provider(namespace = "...")]` und
            /// `harw_extension_api::registry::ExtensionRegistryBuilder::context_provider_declared`.
            /// Default: der Funktionsname (AW3-03-Nachzug).
            pub const NAMESPACE: &'static str = #namespace_str;

            /// Höchste Vertrauensklasse, die dieser Provider behauptet —
            /// siehe `#[context_provider(trust = ...)]`. Default:
            /// `TrustClass::Data`, die niedrigste Klasse — eine
            /// Sicherheitsentscheidung, kein Bequemlichkeitswert (siehe
            /// Moduldoku, Abschnitt „Namensraum, Vertrauensklasse und
            /// Kosten").
            pub const TRUST: ::harw_extension_api::TrustClass = ::harw_extension_api::TrustClass::#trust_ident;

            #ctor
        }

        impl ::harw_extension_api::ContextProvider for #struct_ident {
            /// Überschreibt `ContextProvider::namespace()` mit
            /// [`Self::NAMESPACE`], damit der **geprüfte** Registrierungsweg
            /// über `ExtensionRegistryBuilder::context_provider` (Arc<dyn
            /// ContextProvider>, ohne Zugriff auf assoziierte Konstanten des
            /// konkreten Typs) denselben deklarierten Namensraum sieht wie
            /// `context_provider_declared`/`DeclaredContextProvider`. Ohne
            /// diese Überschreibung fiele dieser Pfad auf den
            /// Trait-Vorgabewert (`std::any::type_name::<Self>()`) zurück —
            /// nie leer, aber nicht der deklarierte Wert.
            fn namespace(&self) -> &'static str {
                Self::NAMESPACE
            }

            /// Überschreibt `ContextProvider::max_trust()` mit
            /// [`Self::TRUST`] aus demselben Grund wie [`Self::namespace`]
            /// oben. **Der Vorgabewert von `TRUST` bleibt
            /// `TrustClass::Data`** — die niedrigste Klasse, siehe die Doku
            /// an `Self::TRUST` sowie
            /// `harw_extension_api::contributors::ContextProvider::max_trust`:
            /// ein Provider ohne `trust`-Angabe behauptet nichts und wird
            /// deshalb „bis zum Beweis des Gegenteils nicht besonders
            /// vertrauenswürdig" behandelt. Diese Methode überschreibt nur
            /// den *Übertragungsweg* zum Trait; sie ändert nichts an der
            /// Vorgabe selbst.
            fn max_trust(&self) -> ::harw_extension_api::TrustClass {
                Self::TRUST
            }

            /// Delegiert an die mit `#[context_provider]` annotierte
            /// Funktion und reicht `ctx` (sowie bei einem zustandsbehafteten
            /// Provider `state`) unverändert weiter.
            ///
            /// # Returns
            /// Der `Vec<ContextFragment>`, den die eingebettete Funktion
            /// liefert.
            fn contribute<'a>(
                &'a self,
                ctx: &'a ::harw_extension_api::TurnInputContext,
            ) -> ::harw_extension_api::ExtFuture<'a, ::std::vec::Vec<::harw_extension_api::ContextFragment>>
            {
                ::std::boxed::Box::pin(#call_expr)
            }
        }

        impl ::harw_extension_api::registry::DeclaredContextProvider for #struct_ident {
            /// Gibt [`Self::NAMESPACE`] zurück — siehe dortige Doku.
            fn declared_namespace(&self) -> &'static str {
                Self::NAMESPACE
            }

            /// Gibt [`Self::TRUST`] zurück — siehe dortige Doku.
            fn declared_max_trust(&self) -> ::harw_extension_api::TrustClass {
                Self::TRUST
            }
        }
    })
}

// ── `#[instructions_provider]` ───────────────────────────────────────────────

/// Verwandelt eine `async fn([state: &StateType]) -> LoadedInstructions` in eine
/// (Unit- oder Zustands-)Struct plus `impl harw_extension_api::InstructionsProvider`.
///
/// Im Unterschied zu [`expand_context_provider`] bekommt die Funktion **kein**
/// `ctx`-Argument — `InstructionsProvider::load` nimmt keinen Kontext entgegen.
///
/// # Errors
/// - nicht-`async fn`
/// - falsche Parameteranzahl (weder 0 noch 1)
/// - der einzige Parameter (`state`), falls vorhanden, ist kein Referenztyp
///
/// # Design-Doc-Referenz
/// AP W1-26f, Teil 1, Makro 2.
pub(crate) fn expand_instructions_provider(
    func: ItemFn,
    args: ContributorArgs,
) -> syn::Result<proc_macro2::TokenStream> {
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[instructions_provider] requires an `async fn`",
        ));
    }

    let mut inputs = func.sig.inputs.iter();
    let state_arg = inputs.next();
    if inputs.next().is_some() {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[instructions_provider] accepts at most `(state: &StateType)`",
        ));
    }

    let state_type = match state_arg {
        Some(arg) => Some(reference_inner_type(arg).ok_or_else(|| {
            syn::Error::new_spanned(
                arg,
                "the first `#[instructions_provider]` argument (state) must be a reference \
                 (e.g. `&StateType`)",
            )
        })?),
        None => None,
    };

    let fn_ident = func.sig.ident.clone();
    let fn_name_str = fn_ident.to_string();
    let struct_ident = resolve_struct_ident(args.struct_name.as_ref(), &fn_name_str)?;
    let name_str = args
        .name
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| fn_name_str.clone());

    // Siehe die identische Begründung in `expand_context_provider`: die
    // annotierte Funktion ist privat und ihre `#[doc]`-Attribute müssen daher
    // auf die generierte (öffentliche) Struct übertragen werden.
    let doc_attrs: Vec<&syn::Attribute> = func
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .collect();

    let (struct_def, ctor, call_expr) = match &state_type {
        Some(ty) => (
            quote! {
                #(#doc_attrs)*
                #[derive(Debug)]
                pub struct #struct_ident {
                    state: #ty,
                }
            },
            quote! {
                /// Erstellt eine neue Instanz, die den übergebenen `state`
                /// kapselt.
                ///
                /// # Arguments
                /// - `state`: wird bei jedem Aufruf unverändert per Referenz
                ///   an die mit `#[instructions_provider]` annotierte
                ///   Funktion weitergereicht.
                ///
                /// # Returns
                /// Eine registrierbare `InstructionsProvider`-Instanz.
                pub fn new(state: #ty) -> Self {
                    Self { state }
                }
            },
            quote! { #fn_ident(&self.state) },
        ),
        None => (
            quote! {
                #(#doc_attrs)*
                #[derive(Debug, Clone, Copy, Default)]
                pub struct #struct_ident;
            },
            quote! {
                /// Erstellt eine neue Instanz dieses zustandslosen Providers.
                ///
                /// # Returns
                /// Eine registrierbare `InstructionsProvider`-Instanz.
                pub fn new() -> Self {
                    Self
                }
            },
            quote! { #fn_ident() },
        ),
    };

    Ok(quote! {
        #func

        #struct_def

        impl #struct_ident {
            /// Maschinenlesbarer Contributor-Name (nur Doku/Tracing) — siehe
            /// `#[instructions_provider(name = "...")]`. Default ist der Funktionsname.
            pub const NAME: &'static str = #name_str;

            #ctor
        }

        impl ::harw_extension_api::InstructionsProvider for #struct_ident {
            /// Delegiert an die mit `#[instructions_provider]` annotierte
            /// Funktion und reicht bei einem zustandsbehafteten Provider
            /// `state` unverändert weiter.
            ///
            /// # Returns
            /// Die `LoadedInstructions`, die die eingebettete Funktion
            /// liefert.
            fn load<'a>(
                &'a self,
            ) -> ::harw_extension_api::ExtFuture<'a, ::harw_extension_api::LoadedInstructions> {
                ::std::boxed::Box::pin(#call_expr)
            }
        }
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod contributor_tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Entfernt sämtliche Whitespaces aus einem `TokenStream::to_string()`, damit
    /// Substring-Prüfungen unabhängig von `quote`s Token-Abstandsregeln sind.
    fn normalize(ts: &proc_macro2::TokenStream) -> String {
        ts.to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    /// Prüft, ob unmittelbar vor der erzeugten Struct ein `#[doc]`-Attribut
    /// steht.
    ///
    /// Bewusst strukturell statt über die exakte Zeichenkette: `proc_macro2`
    /// rendert String-Literale je nach Inhalt als `"..."` oder `r"..."`. Das
    /// ist eine Eigenschaft des Renderings, nicht der Semantik — ein Test
    /// darauf würde bei einer harmlosen Änderung des Doc-Texts brechen.
    fn doc_precedes_struct(flat: &str, struct_name: &str) -> bool {
        let Some(pos) = flat.find(&format!("pubstruct{struct_name};")) else {
            return false;
        };
        let head = &flat[..pos];
        // Zwischen `#[doc...]` und `pub struct` darf nur das Derive stehen.
        match head.rfind("#[doc=") {
            Some(doc_at) => !head[doc_at..].contains("pubstruct"),
            None => false,
        }
    }

    // ── parse_context_provider_args / parse_instructions_provider_args ───────

    #[test]
    fn parse_context_provider_args_accepts_name_and_struct_name() -> TestResult {
        let args = parse_context_provider_args(quote! {
            name = "goal", struct_name = "CustomProvider"
        })
        .map_err(ctx("supported fields should parse"))?;

        assert_eq!(
            args.name.as_ref().map(LitStr::value),
            Some("goal".to_owned())
        );
        assert_eq!(
            args.struct_name.as_ref().map(LitStr::value),
            Some("CustomProvider".to_owned())
        );
        Ok(())
    }

    #[test]
    fn parse_context_provider_args_rejects_unknown_field() -> TestResult {
        let Err(error) = parse_context_provider_args(quote!(unknown = "x")) else {
            return Err(TestError::Unexpected(
                "unknown field must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("unknown `context_provider` attribute field")
        );
        Ok(())
    }

    #[test]
    fn parse_context_provider_args_rejects_duplicate_name() -> TestResult {
        let Err(error) = parse_context_provider_args(quote!(name = "a", name = "b")) else {
            return Err(TestError::Unexpected(
                "duplicate field must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("duplicate `context_provider` attribute field `name`")
        );
        Ok(())
    }

    #[test]
    fn parse_instructions_provider_args_accepts_fields() -> TestResult {
        let args = parse_instructions_provider_args(quote! {
            name = "baseline"
        })
        .map_err(ctx("supported fields should parse"))?;
        assert_eq!(
            args.name.as_ref().map(LitStr::value),
            Some("baseline".to_owned())
        );
        Ok(())
    }

    #[test]
    fn parse_instructions_provider_args_rejects_unknown_field() -> TestResult {
        let Err(error) = parse_instructions_provider_args(quote!(role = "x")) else {
            return Err(TestError::Unexpected(
                "unknown field must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("unknown `instructions_provider` attribute field")
        );
        Ok(())
    }

    // ── expand_context_provider: struct-name derivation ───────────────────────

    #[test]
    fn expand_context_provider_stateless_derives_struct_name_from_fn() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(flat.contains("pubstructGoalContextProvider;"));
        assert!(flat.contains("implGoalContextProvider"));
        assert!(flat.contains("pubfnnew()->Self"));
        assert!(flat.contains("impl::harw_extension_api::ContextProviderforGoalContextProvider"));
        assert!(flat.contains("goal_context(ctx)"));
        Ok(())
    }

    #[test]
    fn expand_context_provider_stateful_generates_state_field() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext, state: &GoalContextState) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateful fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(flat.contains("pubstructGoalContextProvider{state:GoalContextState,}"));
        assert!(flat.contains("pubfnnew(state:GoalContextState)->Self{Self{state}}"));
        assert!(flat.contains("goal_context(ctx,&self.state)"));
        Ok(())
    }

    #[test]
    fn expand_context_provider_struct_name_override_is_used() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let args = parse_context_provider_args(quote!(struct_name = "CustomProvider"))
            .map_err(ctx("struct_name override should parse"))?;
        let tokens = expand_context_provider(func, args).map_err(ctx("override must expand"))?;
        let flat = normalize(&tokens);

        assert!(flat.contains("pubstructCustomProvider;"));
        assert!(!flat.contains("GoalContextProvider"));
        Ok(())
    }

    #[test]
    fn expand_context_provider_default_name_const_falls_back_to_fn_name() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains("pubconstNAME:&'staticstr=\"goal_context\""));
        Ok(())
    }

    // ── expand_context_provider: error cases ──────────────────────────────────

    #[test]
    fn expand_context_provider_rejects_non_async_fn() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let Err(error) = expand_context_provider(func, ContextProviderArgs::default()) else {
            return Err(TestError::Unexpected(
                "non-async fn must be rejected".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "#[context_provider] requires an `async fn`"
        );
        Ok(())
    }

    #[test]
    fn expand_context_provider_rejects_zero_params() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context() -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let Err(error) = expand_context_provider(func, ContextProviderArgs::default()) else {
            return Err(TestError::Unexpected(
                "zero params must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("requires at least `(ctx: &TurnInputContext)`")
        );
        Ok(())
    }

    #[test]
    fn expand_context_provider_rejects_too_many_params() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(
                ctx: &TurnInputContext,
                state: &GoalContextState,
                extra: &ExtraState,
            ) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let Err(error) = expand_context_provider(func, ContextProviderArgs::default()) else {
            return Err(TestError::Unexpected(
                "more than two params must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("accepts at most `(ctx: &TurnInputContext, state: &StateType)`")
        );
        Ok(())
    }

    #[test]
    fn expand_context_provider_rejects_first_param_not_reference() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let Err(error) = expand_context_provider(func, ContextProviderArgs::default()) else {
            return Err(TestError::Unexpected(
                "owned first param must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("the first `#[context_provider]` argument must be a reference")
        );
        Ok(())
    }

    #[test]
    fn expand_context_provider_rejects_second_param_not_reference() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext, state: GoalContextState) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let Err(error) = expand_context_provider(func, ContextProviderArgs::default()) else {
            return Err(TestError::Unexpected(
                "owned second param must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("the second `#[context_provider]` argument (state) must be a reference")
        );
        Ok(())
    }

    // ── expand_instructions_provider ──────────────────────────────────────────

    #[test]
    fn expand_instructions_provider_stateless_derives_struct_name_from_fn() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn baseline_instructions() -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let tokens = expand_instructions_provider(func, ContributorArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(flat.contains("pubstructBaselineInstructionsProvider;"));
        assert!(flat.contains(
            "impl::harw_extension_api::InstructionsProviderforBaselineInstructionsProvider"
        ));
        assert!(flat.contains("baseline_instructions()"));
        Ok(())
    }

    #[test]
    fn expand_instructions_provider_stateful_generates_state_field() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn baseline_instructions(state: &BaselineState) -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let tokens = expand_instructions_provider(func, ContributorArgs::default())
            .map_err(ctx("valid stateful fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(flat.contains("pubstructBaselineInstructionsProvider{state:BaselineState,}"));
        assert!(flat.contains("baseline_instructions(&self.state)"));
        Ok(())
    }

    #[test]
    fn expand_instructions_provider_rejects_non_async_fn() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            fn baseline_instructions() -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let Err(error) = expand_instructions_provider(func, ContributorArgs::default()) else {
            return Err(TestError::Unexpected(
                "non-async fn must be rejected".to_owned(),
            ));
        };
        assert_eq!(
            error.to_string(),
            "#[instructions_provider] requires an `async fn`"
        );
        Ok(())
    }

    #[test]
    fn expand_instructions_provider_rejects_too_many_params() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn baseline_instructions(state: &BaselineState, extra: &ExtraState) -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let Err(error) = expand_instructions_provider(func, ContributorArgs::default()) else {
            return Err(TestError::Unexpected(
                "more than one param must be rejected".to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("accepts at most `(state: &StateType)`")
        );
        Ok(())
    }

    #[test]
    fn expand_instructions_provider_rejects_param_not_reference() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn baseline_instructions(state: BaselineState) -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let Err(error) = expand_instructions_provider(func, ContributorArgs::default()) else {
            return Err(TestError::Unexpected(
                "owned state param must be rejected".to_owned(),
            ));
        };
        assert!(
            error.to_string().contains(
                "the first `#[instructions_provider]` argument (state) must be a reference"
            )
        );
        Ok(())
    }

    #[test]
    fn expand_instructions_provider_struct_name_override_is_used() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn baseline_instructions() -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let args = parse_instructions_provider_args(quote!(struct_name = "CustomInstructions"))
            .map_err(ctx("struct_name override should parse"))?;
        let tokens =
            expand_instructions_provider(func, args).map_err(ctx("override must expand"))?;
        let flat = normalize(&tokens);

        assert!(flat.contains("pubstructCustomInstructions;"));
        assert!(!flat.contains("BaselineInstructionsProvider"));
        Ok(())
    }

    // ── Doc-Durchreichung (AP: Migrationsblocker 2) ───────────────────────────

    /// Belegt, dass die `#[doc]`-Attribute der annotierten Funktion auf der
    /// generierten Struct landen — nicht nur auf der (privaten, in
    /// öffentlichem rustdoc nie sichtbaren) Funktion selbst. Die Prüfung
    /// verlangt exakte Adjazenz zum `pub struct`, damit der Test scheitern
    /// würde, wenn die Doku nur auf `#func` verbliebe.
    #[test]
    fn expand_context_provider_forwards_fn_doc_to_generated_struct() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            /// Emits project metadata fragments.
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("documented fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(
            flat.contains("#[derive(Debug,Clone,Copy,Default)]pubstructGoalContextProvider;")
                && doc_precedes_struct(&flat, "GoalContextProvider"),
            "doc comment of the annotated fn must be forwarded directly onto \
             the generated struct, immediately preceding its derive/definition"
        );
        Ok(())
    }

    /// Dieselbe Prüfung für `#[instructions_provider]`.
    #[test]
    fn expand_instructions_provider_forwards_fn_doc_to_generated_struct() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            /// Loads the baseline system prompt.
            async fn baseline_instructions() -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let tokens = expand_instructions_provider(func, ContributorArgs::default())
            .map_err(ctx("documented fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(
            flat.contains(
                "#[derive(Debug,Clone,Copy,Default)]pubstructBaselineInstructionsProvider;"
            ) && doc_precedes_struct(&flat, "BaselineInstructionsProvider"),
            "doc comment of the annotated fn must be forwarded directly onto \
             the generated struct, immediately preceding its derive/definition"
        );
        Ok(())
    }

    // ── Derive-Wahl für zustandsbehaftete Provider ────────────────────────────

    /// Der zustandsbehaftete Zweig darf `Debug` ableiten (im Gegensatz zu
    /// `Clone`/`Copy`/`Default`, die für einen beliebigen `state`-Typ nicht
    /// garantiert werden können) — siehe Moduldoc, Punkt 1.
    #[test]
    fn expand_context_provider_stateful_derives_debug() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext, state: &GoalContextState) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateful fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(
            flat.contains("#[derive(Debug)]pubstructGoalContextProvider{state:GoalContextState,}"),
            "stateful provider must derive Debug for tracing/observability"
        );
        Ok(())
    }

    /// Dieselbe Prüfung für `#[instructions_provider]`.
    #[test]
    fn expand_instructions_provider_stateful_derives_debug() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn baseline_instructions(state: &BaselineState) -> LoadedInstructions {
                LoadedInstructions::default()
            }
        };
        let tokens = expand_instructions_provider(func, ContributorArgs::default())
            .map_err(ctx("valid stateful fn must expand"))?;
        let flat = normalize(&tokens);

        assert!(
            flat.contains(
                "#[derive(Debug)]pubstructBaselineInstructionsProvider{state:BaselineState,}"
            ),
            "stateful provider must derive Debug for tracing/observability"
        );
        Ok(())
    }

    /// Nutzt das ergänzte `Debug`-Derive tatsächlich (nicht nur strukturelle
    /// Token-Prüfung): eine reale zustandsbehaftete Struct mit `Debug`-fähigem
    /// State muss sich mit `{:?}` formatieren lassen.
    #[test]
    fn stateful_provider_debug_derive_is_usable() {
        // `dead_code` erlaubt, weil beide Felder ausschließlich über die
        // `Debug`-Ausgabe gelesen werden — genau das prüft dieser Test unten
        // mit `rendered.contains("demo")`. Die Dead-Code-Analyse zählt ein
        // abgeleitetes `Debug` ausdrücklich nicht als Lesezugriff und sagt das
        // in ihrer eigenen Meldung.
        #[derive(Debug)]
        #[allow(dead_code)]
        pub struct DemoState {
            label: String,
        }

        #[derive(Debug)]
        #[allow(dead_code)]
        pub struct DemoProvider {
            state: DemoState,
        }

        let provider = DemoProvider {
            state: DemoState {
                label: "demo".to_owned(),
            },
        };

        let rendered = format!("{provider:?}");
        assert!(rendered.contains("DemoProvider"));
        assert!(rendered.contains("demo"));
    }

    // ── AW3-03-Nachzug: `namespace`, `trust` ──────────────────────────────────

    #[test]
    fn parse_context_provider_args_accepts_namespace_and_trust() -> TestResult {
        let args = parse_context_provider_args(quote! {
            namespace = "plan", trust = Evidence
        })
        .map_err(ctx("valid namespace/trust must parse"))?;
        assert_eq!(
            args.namespace.as_ref().map(LitStr::value),
            Some("plan".to_owned())
        );
        assert_eq!(
            args.trust.as_ref().map(Ident::to_string),
            Some("Evidence".to_owned())
        );
        Ok(())
    }

    #[test]
    fn parse_context_provider_args_rejects_empty_namespace() -> TestResult {
        let Err(error) = parse_context_provider_args(quote!(namespace = "")) else {
            return Err(TestError::Unexpected(
                "empty namespace must be rejected".to_owned(),
            ));
        };
        assert!(error.to_string().contains("must not be empty"));
        Ok(())
    }

    #[test]
    fn parse_context_provider_args_rejects_namespace_with_invalid_character() -> TestResult {
        let Err(error) = parse_context_provider_args(quote!(namespace = "My.Namespace")) else {
            return Err(TestError::Unexpected(
                "uppercase and '.' must be rejected".to_owned(),
            ));
        };
        assert!(error.to_string().contains("invalid character"));
        Ok(())
    }

    #[test]
    fn parse_context_provider_args_rejects_unknown_trust_class() -> TestResult {
        let Err(error) = parse_context_provider_args(quote!(trust = Bogus)) else {
            return Err(TestError::Unexpected(
                "unknown trust class must be rejected".to_owned(),
            ));
        };
        assert!(error.to_string().contains("unknown trust class `Bogus`"));
        Ok(())
    }

    #[test]
    fn expand_context_provider_default_namespace_is_fn_name() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains("pubconstNAMESPACE:&'staticstr=\"goal_context\""));
        Ok(())
    }

    #[test]
    fn expand_context_provider_explicit_namespace_is_used() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let args = parse_context_provider_args(quote!(namespace = "plan"))
            .map_err(ctx("namespace override should parse"))?;
        let tokens =
            expand_context_provider(func, args).map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains("pubconstNAMESPACE:&'staticstr=\"plan\""));
        Ok(())
    }

    /// Belegt, dass die Vorgabe für `trust` nachweislich die niedrigste Klasse
    /// ist (`TrustClass::Data`) — eine Sicherheitsentscheidung, siehe
    /// Moduldoku. Ein Provider, der `trust` nicht angibt, darf niemals still
    /// `Instruction` oder `Evidence` bekommen.
    #[test]
    fn expand_context_provider_default_trust_is_lowest_class() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains(
            "pubconstTRUST:::harw_extension_api::TrustClass=::harw_extension_api::TrustClass::Data;"
        ));
        Ok(())
    }

    #[test]
    fn expand_context_provider_explicit_trust_is_used() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let args = parse_context_provider_args(quote!(trust = Instruction))
            .map_err(ctx("trust override should parse"))?;
        let tokens =
            expand_context_provider(func, args).map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains(
            "pubconstTRUST:::harw_extension_api::TrustClass=::harw_extension_api::TrustClass::Instruction;"
        ));
        Ok(())
    }

    /// Belegt, dass jeder generierte Provider zusätzlich `DeclaredContextProvider`
    /// implementiert — der Weg, auf dem `namespace`/`trust` einen typgelöschten
    /// `Arc<dyn ContextProvider>` überleben (siehe Moduldoku, Abschnitt „Wie
    /// NAMESPACE/TRUST bei der Registrierung ankommen").
    #[test]
    fn expand_context_provider_implements_declared_context_provider() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains(
            "impl::harw_extension_api::registry::DeclaredContextProviderforGoalContextProvider"
        ));
        assert!(flat.contains("fndeclared_namespace(&self)->&'staticstr{Self::NAMESPACE}"));
        assert!(flat.contains(
            "fndeclared_max_trust(&self)->::harw_extension_api::TrustClass{Self::TRUST}"
        ));
        Ok(())
    }

    // ── Nachzug: `ContextProvider::namespace()`/`max_trust()` selbst ──────────
    // überschrieben (statt nur `DeclaredContextProvider`), siehe Moduldoku,
    // Abschnitt „Wie NAMESPACE/TRUST bei der Registrierung ankommen".

    /// Der geprüfte Registrierungsweg über `ContextProvider::max_trust()`
    /// (nicht nur `DeclaredContextProvider::declared_max_trust()`) muss die
    /// deklarierte Klasse melden, hier `Evidence` — der Kernbefund, den
    /// dieser Nachzug behebt.
    #[test]
    fn expand_context_provider_overrides_context_provider_max_trust_with_declared_value()
    -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let args = parse_context_provider_args(quote!(trust = Evidence))
            .map_err(ctx("trust override should parse"))?;
        let tokens =
            expand_context_provider(func, args).map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains("impl::harw_extension_api::ContextProviderforGoalContextProvider"));
        assert!(flat.contains("fnmax_trust(&self)->::harw_extension_api::TrustClass{Self::TRUST}"));
        Ok(())
    }

    /// Ohne `trust`-Angabe hält die Vorgabe: `Self::TRUST` bleibt
    /// `TrustClass::Data`, und `ContextProvider::max_trust()` gibt weiterhin
    /// nur `Self::TRUST` zurück — kein erfundener Wert, kein Typname als
    /// Klasse.
    #[test]
    fn expand_context_provider_overrides_context_provider_max_trust_defaults_to_data() -> TestResult
    {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains(
            "pubconstTRUST:::harw_extension_api::TrustClass=::harw_extension_api::TrustClass::Data;"
        ));
        assert!(flat.contains("fnmax_trust(&self)->::harw_extension_api::TrustClass{Self::TRUST}"));
        Ok(())
    }

    /// Ein deklarierter `namespace` muss über `ContextProvider::namespace()`
    /// selbst ankommen, nicht nur über `DeclaredContextProvider`.
    #[test]
    fn expand_context_provider_overrides_context_provider_namespace_with_declared_value()
    -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let args = parse_context_provider_args(quote!(namespace = "plan"))
            .map_err(ctx("namespace override should parse"))?;
        let tokens =
            expand_context_provider(func, args).map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains("pubconstNAMESPACE:&'staticstr=\"plan\""));
        assert!(flat.contains("fnnamespace(&self)->&'staticstr{Self::NAMESPACE}"));
        Ok(())
    }

    /// Ohne `namespace`-Angabe fällt `NAMESPACE` auf den Funktionsnamen
    /// zurück (nicht leer) — und `ContextProvider::namespace()` überschreibt
    /// weiterhin mit `Self::NAMESPACE`, nicht mit dem Trait-Vorgabewert
    /// (`std::any::type_name::<Self>()`).
    #[test]
    fn expand_context_provider_overrides_context_provider_namespace_defaults_to_fn_name()
    -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let tokens = expand_context_provider(func, ContextProviderArgs::default())
            .map_err(ctx("valid stateless fn must expand"))?;
        let flat = normalize(&tokens);
        assert!(flat.contains("pubconstNAMESPACE:&'staticstr=\"goal_context\""));
        assert!(flat.contains("fnnamespace(&self)->&'staticstr{Self::NAMESPACE}"));
        Ok(())
    }

    #[test]
    fn expand_context_provider_rejects_uppercase_default_namespace() -> TestResult {
        let func: ItemFn = syn::parse_quote! {
            async fn GoalContext(ctx: &TurnInputContext) -> Vec<ContextFragment> {
                Vec::new()
            }
        };
        let Err(error) = expand_context_provider(func, ContextProviderArgs::default()) else {
            return Err(TestError::Unexpected(
                "an uppercase fn name must not silently become a broken default namespace"
                    .to_owned(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("is not a valid default namespace")
        );
        Ok(())
    }
}
