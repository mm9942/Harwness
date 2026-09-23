//! `#[derive(SensorSource)]`-Expansion — Boilerplate für die elf Sensor-Crates.
//!
//! Spec: AW2-06-Brief (`#[derive(SensorSource)]`). Dritter von vier
//! Makro-Knoten nach AW0-02 (`field!`, `metrics!`, `Redact`) und AW1-02
//! (`#[traced]`); vor AW5-01 (`warden_actions!`).
//!
//! # Zweck
//! Elf Sensor-Crates implementieren `harw_dod_signals::Sensor` fast
//! identisch: `handle()` gibt das gebundene `SensorHandle<Bound>` zurück,
//! `poll()` liest eine glob-adressierte Menge von `/proc`- oder
//! `/sys`-Dateien über `harw-dod-readfs`, baut daraus `HostSample`s und
//! reicht `now` unverändert durch. Ohne dieses Makro würde diese Form elfmal
//! von Hand geschrieben — und laut Aufgabenstellung hat genau dieser
//! Wiederholungsfehler im Vorprogramm 26 Compile-Fehler auf einmal erzeugt.
//! Dieses Modul erzeugt sie einmal.
//!
//! # Was das Makro NICHT abdeckt
//! Sensoren, die `SecurityEvent`s erzeugen (Audit-Netlink, Journal,
//! Dateisystem-Watch) oder deren Quelle kein glob-adressierbares
//! Dateisystemmuster ist, schreiben ihre `Sensor`-Implementierung von Hand.
//! Dieses Makro deckt den häufigen Fall ab: eine glob-adressierte Menge
//! numerischer sysfs-/procfs-Werte, ein `HostSample` je Treffer.
//!
//! # Die drei erzwungenen Regeln (Moduldoku-Abschnitt, siehe Brief)
//! 1. **Genau eine Capability.** [`parse_sensor_args`] lehnt eine zweite
//!    `capability = ...`-Angabe in derselben Deklaration ab.
//! 2. **Kein `std::fs`.** Der erzeugte `poll()`-Körper referenziert
//!    ausschließlich `::harw_dod_readfs::glob::glob`,
//!    `::harw_dod_readfs::parse_i64` und `::harw_dod_readfs::parse_u64`.
//! 3. **Injizierte Zeit.** Das erzeugte `poll(&self, now: ::jiff::Timestamp)`
//!    reicht `now` unverändert in jedes erzeugte `HostSample` durch; die
//!    Systemuhr wird an keiner Stelle gelesen.
//!
//! # Attributgrammatik
//!
//! ```ignore
//! #[derive(harw_macros::SensorSource)]
//! #[sensor(
//!     capability = ReadSysfsThermal,
//!     id = "thermal",
//!     metrics = "harw_dod_thermal_",
//!     error = "path::to::LocalError",   // optional
//! )]
//! struct ThermalSensor {
//!     handle: harw_dod_cap::SensorHandle<harw_dod_cap::Bound>,
//!     #[source(glob = "thermal_zone*/temp", parse = "i64", metric = "temperature_celsius")]
//!     zones: (),
//! }
//! ```
//!
//! Struct-Attribut `#[sensor(...)]` (genau einmal wirksam je Struct):
//! - `capability = <Ident>` — **erforderlich**, genau einmal. Einer der
//!   vierzehn Bezeichner aus [`CAPABILITY_VARIANTS`], wörtlich wie die
//!   Variante in `harw_dod_cap::Capability`.
//! - `id = "..."` — **erforderlich**. Erzeugt `Self::SENSOR_ID`.
//! - `metrics = "..."` — **erforderlich**. Namenspräfix für die beiden vom
//!   Makro deklarierten Zähler (siehe unten).
//! - `error = "pfad::zu::LocalError"` — **optional**. Siehe
//!   [`expand_sensor_source`], Abschnitt „`From`-Brücke".
//!
//! Feld-Attribut `#[source(...)]` (genau ein Feld der Struct trägt es):
//! - `glob = "..."` — **erforderlich**, nicht leer. Ein **Suffix relativ zur
//!   Bereichswurzel**, also etwa `"thermal_zone*/temp"`, **nicht**
//!   `"sys/class/thermal/thermal_zone*/temp"`.
//!
//!   **Warum relativ, und warum das wichtig ist.** Die erste Fassung dieses
//!   Makros verdrahtete das Muster zur Compile-Zeit als vollen Pfad. Das ist
//!   still falsch: `harw_dod_readfs::glob::glob` sucht ab dem echten `/`, und
//!   der `ReadScope` wirkt nur als nachträglicher Filter. Ein fester voller
//!   Pfad findet deshalb **in Produktion** etwas — die Bereichswurzel ist
//!   zufällig derselbe Pfad — und **in der Fixture-Prüfung nichts**, weil die
//!   Wurzel dort `fixtures/<fall>/tree` ist. Das Ergebnis wäre kein Fehler,
//!   sondern ein leeres `SensorReading`: ein Sensor, der in Produktion misst
//!   und im Test schweigt, und dessen sechs Harness-Prüfungen alle grün
//!   melden, ohne je etwas ausgeführt zu haben.
//!
//!   Der erzeugte `poll` baut das vollständige Muster deshalb **zur Laufzeit**
//!   aus `scope.roots().next()` plus diesem Suffix. Ein Bereich ohne Wurzel
//!   liefert eine leere Trefferliste, keinen Fehler.
//!
//!   Absolute Muster oder `..`-Komponenten prüft **ausschließlich** `glob()`
//!   selbst zur Laufzeit — dieses Makro wiederholt die Prüfung nicht. Die
//!   einzige hier zusätzlich abgelehnte Form ist ein **leeres** Muster.
//! - `parse = "i64" | "u64"` — **erforderlich**. Wählt
//!   `harw_dod_readfs::parse_i64` bzw. `parse_u64` für jeden Treffer.
//! - `metric = "..."` — **erforderlich**. Der `HostSample::metric`-Wert für
//!   jeden Treffer dieses Feldes.
//!
//! Der Feldtyp selbst ist dem Makro gleichgültig — es liest den Feldwert
//! nie. Konvention (siehe Beispiel oben): `()`, ein reiner Attribut-Anker.
//!
//! # Fehler
//! [`expand_sensor_source`] liefert `syn::Error`, nie einen Panic. Siehe
//! dessen Dokumentation für die vollständige Liste der Compile-Fehler.
//!
//! # Design-doc reference
//! AW2-06-Brief (`#[derive(SensorSource)]`).

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Field, Fields, Ident, LitStr, Path};

/// Die vierzehn erlaubten `harw_dod_cap::Capability`-Variantennamen.
///
/// `harw-macros` hängt nicht produktiv von `harw-dod-cap` ab (siehe
/// Moduldoku von `lib.rs`) — diese Liste ist deshalb eine bewusst gepflegte
/// Textkopie der Variantennamen aus `harw-dod-cap/src/capability.rs`, nicht
/// eine Typprüfung. Eine neue Capability in `harw-dod-cap` erfordert also
/// eine bewusste, sichtbare Ergänzung hier.
const CAPABILITY_VARIANTS: &[&str] = &[
    "ReadSysfsThermal",
    "ReadProcStat",
    "ReadProcMeminfo",
    "ReadSysfsBlock",
    "ReadProcNetDev",
    "ReadSysfsDrm",
    "ReadCgroupV2",
    "ReadProcNet",
    "ReadJournal",
    "ReadAuditNetlink",
    "ReadScanReports",
    "ReadWorkspaceGraph",
    "WatchFilesystem",
    "LoadBpfProgram",
];

/// Prüft, ob `ident` einer der vierzehn erlaubten Capability-Namen ist.
///
/// # Errors
/// `syn::Error`, gespannt auf `ident`, wenn der Name in
/// [`CAPABILITY_VARIANTS`] nicht vorkommt. Die Meldung listet alle
/// erlaubten Namen, damit ein Tippfehler nicht zu einer stillen Rückfrage
/// beim Aufrufer führt.
///
/// # Design-doc reference
/// AW2-06-Brief, Regel 1 („Genau eine Capability je Sensor").
fn validate_capability(ident: &Ident) -> syn::Result<()> {
    let name = ident.to_string();
    if CAPABILITY_VARIANTS.contains(&name.as_str()) {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            ident,
            format!(
                "unbekannte capability `{name}`; erwartet eine von: {}",
                CAPABILITY_VARIANTS.join(", ")
            ),
        ))
    }
}

/// Geparste `#[sensor(...)]`-Konfiguration eines Structs.
struct SensorArgs {
    /// Die deklarierte Fähigkeit (bereits gegen [`CAPABILITY_VARIANTS`]
    /// geprüft).
    capability: Ident,
    /// `#[sensor(id = "...")]` — wird zu `Self::SENSOR_ID`.
    sensor_id: LitStr,
    /// `#[sensor(metrics = "...")]` — Namenspräfix der beiden Zähler.
    metrics_prefix: LitStr,
    /// `#[sensor(error = "...")]`, falls angegeben.
    error_path: Option<Path>,
}

/// Liest alle `#[sensor(...)]`-Attribute eines Items ein.
///
/// Mehrere `#[sensor(...)]`-Attribute auf demselben Item werden
/// zusammengeführt (wie bei `#[harw_id(...)]`, siehe `id.rs`); ein Schlüssel
/// darf über alle Vorkommen hinweg aber nur genau einmal erscheinen.
///
/// # Errors
/// - `capability`, `id`, `metrics` oder `error` doppelt angegeben →
///   `syn::Error`.
/// - `capability` ist kein gültiger Variantenname → siehe
///   [`validate_capability`].
/// - `error` ist kein gültiger Pfad → `syn::Error`.
/// - unbekannter Schlüssel → `syn::Error`.
/// - eines der erforderlichen Felder (`capability`, `id`, `metrics`) fehlt
///   am Ende → `syn::Error`, gespannt auf `struct_ident`.
///
/// # Design-doc reference
/// AW2-06-Brief, Abschnitt „Attributgrammatik" (Moduldoku oben).
fn parse_sensor_args(attrs: &[Attribute], struct_ident: &Ident) -> syn::Result<SensorArgs> {
    let mut capability: Option<Ident> = None;
    let mut sensor_id: Option<LitStr> = None;
    let mut metrics_prefix: Option<LitStr> = None;
    let mut error_path: Option<Path> = None;

    for attr in attrs {
        if !attr.path().is_ident("sensor") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("capability") {
                if capability.is_some() {
                    return Err(meta.error("`capability` doppelt angegeben"));
                }
                let ident: Ident = meta.value()?.parse()?;
                validate_capability(&ident)?;
                capability = Some(ident);
                Ok(())
            } else if meta.path.is_ident("id") {
                if sensor_id.is_some() {
                    return Err(meta.error("`id` doppelt angegeben"));
                }
                let lit: LitStr = meta.value()?.parse()?;
                sensor_id = Some(lit);
                Ok(())
            } else if meta.path.is_ident("metrics") {
                if metrics_prefix.is_some() {
                    return Err(meta.error("`metrics` doppelt angegeben"));
                }
                let lit: LitStr = meta.value()?.parse()?;
                metrics_prefix = Some(lit);
                Ok(())
            } else if meta.path.is_ident("error") {
                if error_path.is_some() {
                    return Err(meta.error("`error` doppelt angegeben"));
                }
                let lit: LitStr = meta.value()?.parse()?;
                let path: Path = syn::parse_str(&lit.value()).map_err(|e| {
                    syn::Error::new_spanned(
                        &lit,
                        format!("`error` muss ein gültiger Pfad sein: {e}"),
                    )
                })?;
                error_path = Some(path);
                Ok(())
            } else {
                Err(meta.error(
                    "unbekannter sensor-Schlüssel; erwartet: capability, id, metrics, error",
                ))
            }
        })?;
    }

    let capability = capability.ok_or_else(|| {
        syn::Error::new_spanned(struct_ident, "fehlendes `#[sensor(capability = ...)]`")
    })?;
    let sensor_id = sensor_id.ok_or_else(|| {
        syn::Error::new_spanned(struct_ident, "fehlendes `#[sensor(id = \"...\")]`")
    })?;
    let metrics_prefix = metrics_prefix.ok_or_else(|| {
        syn::Error::new_spanned(struct_ident, "fehlendes `#[sensor(metrics = \"...\")]`")
    })?;

    Ok(SensorArgs {
        capability,
        sensor_id,
        metrics_prefix,
        error_path,
    })
}

/// Welche getypte Lesefunktion aus `harw-dod-readfs` ein `#[source(...)]`-Feld
/// pro Treffer verwendet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseMode {
    /// `harw_dod_readfs::parse_i64`.
    I64,
    /// `harw_dod_readfs::parse_u64`.
    U64,
}

impl ParseMode {
    /// Der volle Pfad der gewählten Lesefunktion, als Tokens.
    fn reader_path(self) -> TokenStream {
        match self {
            Self::I64 => quote! { ::harw_dod_readfs::parse_i64 },
            Self::U64 => quote! { ::harw_dod_readfs::parse_u64 },
        }
    }
}

/// Geparste `#[source(...)]`-Konfiguration eines Feldes.
struct SourceField {
    /// Der annotierte Feldbezeichner (nur zur Erzeugung von `let _ = &self.#ident;`
    /// verwendet, damit das Feld nicht als „nie gelesen" auffällt — sein Wert
    /// selbst ist dem Makro gleichgültig).
    field_ident: Ident,
    /// `#[source(glob = "...")]`.
    glob: LitStr,
    /// `#[source(parse = "...")]`.
    parse_mode: ParseMode,
    /// `#[source(metric = "...")]`.
    metric: LitStr,
}

/// Liest die `#[source(...)]`-Konfiguration eines einzelnen Feldes, falls
/// vorhanden.
///
/// # Returns
/// `None`, wenn `field` kein `#[source(...)]`-Attribut trägt. `Some(..)` mit
/// der vollständigen Konfiguration sonst.
///
/// # Errors
/// - `glob`, `parse` oder `metric` doppelt angegeben → `syn::Error`.
/// - `glob` ist ein leeres String-Literal → `syn::Error` (siehe Moduldoku,
///   Abschnitt zu `glob` — die einzige hier zusätzlich zur Laufzeitprüfung
///   in `glob()` selbst durchgesetzte Regel).
/// - `parse` ist weder `"i64"` noch `"u64"` → `syn::Error`.
/// - unbekannter Schlüssel → `syn::Error`.
/// - eines der erforderlichen Felder fehlt am Ende → `syn::Error`.
///
/// # Design-doc reference
/// AW2-06-Brief, Abschnitt „Attributgrammatik" (Moduldoku oben).
fn parse_source_field(field: &Field) -> syn::Result<Option<SourceField>> {
    if !field.attrs.iter().any(|a| a.path().is_ident("source")) {
        return Ok(None);
    }

    let field_ident = field
        .ident
        .clone()
        .ok_or_else(|| syn::Error::new_spanned(field, "SensorSource erfordert benannte Felder"))?;

    let mut glob: Option<LitStr> = None;
    let mut parse_mode: Option<ParseMode> = None;
    let mut metric: Option<LitStr> = None;

    for attr in &field.attrs {
        if !attr.path().is_ident("source") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("glob") {
                if glob.is_some() {
                    return Err(meta.error("`glob` doppelt angegeben"));
                }
                let lit: LitStr = meta.value()?.parse()?;
                if lit.value().is_empty() {
                    return Err(syn::Error::new_spanned(
                        &lit,
                        "Glob-Muster darf nicht leer sein",
                    ));
                }
                glob = Some(lit);
                Ok(())
            } else if meta.path.is_ident("parse") {
                if parse_mode.is_some() {
                    return Err(meta.error("`parse` doppelt angegeben"));
                }
                let lit: LitStr = meta.value()?.parse()?;
                parse_mode = Some(match lit.value().as_str() {
                    "i64" => ParseMode::I64,
                    "u64" => ParseMode::U64,
                    other => {
                        return Err(syn::Error::new_spanned(
                            &lit,
                            format!("unbekannter parse-Modus `{other}`; erwartet: i64, u64"),
                        ));
                    }
                });
                Ok(())
            } else if meta.path.is_ident("metric") {
                if metric.is_some() {
                    return Err(meta.error("`metric` doppelt angegeben"));
                }
                let lit: LitStr = meta.value()?.parse()?;
                metric = Some(lit);
                Ok(())
            } else {
                Err(meta.error("unbekannter source-Schlüssel; erwartet: glob, parse, metric"))
            }
        })?;
    }

    let glob = glob.ok_or_else(|| {
        syn::Error::new_spanned(&field_ident, "fehlendes `#[source(glob = \"...\")]`")
    })?;
    let parse_mode = parse_mode.ok_or_else(|| {
        syn::Error::new_spanned(&field_ident, "fehlendes `#[source(parse = \"...\")]`")
    })?;
    let metric = metric.ok_or_else(|| {
        syn::Error::new_spanned(&field_ident, "fehlendes `#[source(metric = \"...\")]`")
    })?;

    Ok(Some(SourceField {
        field_ident,
        glob,
        parse_mode,
        metric,
    }))
}

/// Expander für das `#[derive(SensorSource)]`-Makro.
///
/// # Description
/// Siehe die Moduldoku für die vollständige Attributgrammatik. Erzeugt für
/// eine Struct mit einem `handle`-Feld und genau einem `#[source(...)]`-Feld:
///
/// 1. `impl #struct_ident` mit `Self::CAPABILITY` (`harw_dod_cap::Capability`,
///    die maschinenlesbare Deklaration für das CI-Privilegienbudget-Gate),
///    `Self::SENSOR_ID` (`&'static str`) und `record_poll_metrics(&self, sink,
///    outcome)` — der Emissionspfad für die beiden vom Makro deklarierten
///    Zähler.
/// 2. `impl ::harw_dod_signals::Sensor for #struct_ident`: `handle()` gibt
///    `&self.handle` zurück; `poll(&self, now)` ruft
///    `::harw_dod_readfs::glob::glob` auf dem `#[source(...)]`-Muster auf,
///    liest jeden Treffer über `parse_i64`/`parse_u64` (je nach `parse = ...`)
///    und baut daraus `HostSample { sensor: self.handle.id().clone(),
///    observed_at: now, metric: Cow::Borrowed(<metric>), value: <wert as f64> }`.
///    Nie `std::fs`, nie `SystemTime::now()`/`Timestamp::now()`.
/// 3. Ein verstecktes Modul mit den beiden über `metrics!` deklarierten
///    Zählern (`<prefix>polls_total`, `<prefix>errors_total{kind}`) und der
///    Emissionslogik dahinter.
/// 4. `impl ::std::convert::From<LocalError> for ::harw_dod_cap::SensorError`,
///    **nur wenn** `#[sensor(error = "...")]` angegeben ist — die
///    „`From`-Brücke" (siehe unten).
///
/// # `From`-Brücke
/// Ist `#[sensor(error = "path::to::LocalError")]` angegeben, erzeugt das
/// Makro `impl From<LocalError> for ::harw_dod_cap::SensorError { fn
/// from(err: LocalError) -> Self { err.into_sensor_error() } }`. `LocalError`
/// muss dafür selbst eine Methode `fn into_sensor_error(self) ->
/// ::harw_dod_cap::SensorError` bereitstellen (von Hand geschrieben) — dieses
/// Makro kann die domänenspezifische Zuordnung der Varianten von
/// `LocalError` auf die fünf `SensorError`-Varianten nicht kennen. Fehlt die
/// Methode, meldet der Compiler „Methode nicht gefunden" an der erzeugten
/// `impl`-Stelle; es gibt keinen stillen Rückfall (dieselbe Philosophie wie
/// bei `#[traced(fields(...))]`, siehe `traced.rs`). Der Grund, warum dieses
/// `impl` überhaupt in der Sensor-Crate selbst stehen darf, obwohl weder
/// `LocalError` noch `SensorError` dort *beide* fremd wären: die
/// Waisenregel (orphan rule) erlaubt `impl ForeignTrait<LocalType> for
/// ForeignType`, solange der Typparameter des Traits (hier `LocalError`)
/// lokal ist — genau der Fall hier, weil `From<T>` generisch über `T` ist.
///
/// # Errors
/// - Item ist kein Struct (Enum, Union) → `syn::Error`.
/// - Struct hat keine benannten Felder (Tuple-/Unit-Struct) → `syn::Error`.
/// - `#[sensor(...)]`-Konfiguration ungültig → siehe [`parse_sensor_args`].
/// - kein Feld namens `handle` → `syn::Error`.
/// - kein Feld mit `#[source(...)]`, oder mehr als eines → `syn::Error`
///   („ein Sensor mit zwei Quellen ist zwei Sensoren").
/// - `#[source(...)]`-Konfiguration ungültig → siehe [`parse_source_field`].
///
/// # Design-doc reference
/// AW2-06-Brief (`#[derive(SensorSource)]`).
pub(crate) fn expand_sensor_source(input: &DeriveInput) -> syn::Result<TokenStream> {
    let struct_ident = &input.ident;

    let data = match &input.data {
        Data::Struct(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "SensorSource kann nur auf structs angewendet werden, nicht auf enums oder unions",
            ));
        }
    };

    let named = match &data.fields {
        Fields::Named(named) => named,
        _ => {
            return Err(syn::Error::new_spanned(
                &data.fields,
                "SensorSource erfordert eine struct mit benannten Feldern",
            ));
        }
    };

    let args = parse_sensor_args(&input.attrs, struct_ident)?;

    let has_handle_field = named
        .named
        .iter()
        .any(|f| f.ident.as_ref().is_some_and(|i| i == "handle"));
    if !has_handle_field {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "SensorSource erfordert ein Feld \
             `handle: harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`",
        ));
    }

    let mut source_fields = Vec::new();
    for field in &named.named {
        if let Some(source_field) = parse_source_field(field)? {
            source_fields.push(source_field);
        }
    }

    let source = if source_fields.len() == 1 {
        source_fields.remove(0)
    } else if source_fields.is_empty() {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "SensorSource erfordert genau ein Feld mit `#[source(...)]`; keines gefunden",
        ));
    } else {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "SensorSource erlaubt genau ein Feld mit `#[source(...)]`; \
             ein Sensor mit zwei Quellen ist zwei Sensoren",
        ));
    };

    let capability_ident = &args.capability;
    let sensor_id_lit = &args.sensor_id;

    let prefix = args.metrics_prefix.value();
    let polls_name = LitStr::new(&format!("{prefix}polls_total"), args.metrics_prefix.span());
    let errors_name = LitStr::new(&format!("{prefix}errors_total"), args.metrics_prefix.span());

    let metrics_mod_ident = Ident::new(
        &format!("__harw_sensor_source_metrics_{struct_ident}"),
        struct_ident.span(),
    );

    let field_ident = &source.field_ident;
    let glob_lit = &source.glob;
    let metric_lit = &source.metric;
    let reader_path = source.parse_mode.reader_path();

    let sensor_impl = quote! {
        impl #struct_ident {
            /// Die Fähigkeit, die dieser Sensor beansprucht. Maschinenlesbare
            /// Deklaration für das CI-Privilegienbudget-Gate — erzeugt von
            /// `#[derive(harw_macros::SensorSource)]` (AW2-06).
            pub const CAPABILITY: ::harw_dod_cap::Capability =
                ::harw_dod_cap::Capability::#capability_ident;

            /// Kanonische Kennung dieses Sensors, siehe `#[sensor(id = "...")]`.
            pub const SENSOR_ID: &'static str = #sensor_id_lit;

            /// Zählt einen Poll-Versuch und, im Fehlerfall, den Fehler nach
            /// Art, und meldet beide an `sink`. Erzeugt von
            /// `#[derive(harw_macros::SensorSource)]` (AW2-06); der Sentinel
            /// (oder ein anderer Aufrufer, der einen `TelemetrySink` hält)
            /// ruft dies um jeden `poll`-Aufruf herum auf.
            pub fn record_poll_metrics(
                &self,
                sink: &dyn ::harw_observe::TelemetrySink,
                outcome: &::std::result::Result<
                    ::harw_dod_signals::SensorReading,
                    ::harw_dod_cap::SensorError,
                >,
            ) {
                #metrics_mod_ident::record_poll(sink, outcome);
            }
        }

        impl ::harw_dod_signals::Sensor for #struct_ident {
            fn handle(&self) -> &::harw_dod_cap::SensorHandle<::harw_dod_cap::Bound> {
                &self.handle
            }

            fn poll(
                &self,
                now: ::jiff::Timestamp,
            ) -> ::std::result::Result<
                ::harw_dod_signals::SensorReading,
                ::harw_dod_cap::SensorError,
            > {
                // Der Feldwert selbst ist diesem Makro gleichgültig (siehe
                // Moduldoku); dieser Zugriff verhindert nur einen
                // `dead_code`-Hinweis auf das Attribut-Anker-Feld.
                let _ = &self.#field_ident;

                let __harw_scope = self.handle.scope();

                // Das Suchmuster wird **zur Laufzeit** aus der Bereichswurzel
                // gebaut, nicht zur Compile-Zeit fest verdrahtet.
                //
                // Grund: `harw_dod_readfs::glob::glob` sucht ab dem echten
                // `/`; der `ReadScope` wirkt nur als nachträglicher Filter.
                // Ein festes Muster wie "sys/class/thermal/thermal_zone*/temp"
                // fände in Produktion etwas — weil die Bereichswurzel zufällig
                // derselbe Pfad ist — und in einer Fixture-Prüfung, deren
                // Wurzel `fixtures/<fall>/tree` ist, **nichts**. Das Ergebnis
                // wäre kein Fehler, sondern ein still leeres `SensorReading`:
                // ein Sensor, der in Produktion misst und im Test schweigt.
                //
                // Deshalb ist `#[source(glob = "...")]` ein **Suffix relativ
                // zur Bereichswurzel**, kein absoluter Pfad.
                let __harw_paths = match __harw_scope.roots().next() {
                    // Ein Bereich ohne Wurzel kann nichts finden. Das ist
                    // kein Fehler des Sensors, sondern eine leere Menge.
                    ::core::option::Option::None => ::std::vec::Vec::new(),
                    ::core::option::Option::Some(__harw_root) => {
                        let __harw_relative =
                            __harw_root.strip_prefix("/").unwrap_or(__harw_root);
                        let __harw_relative = __harw_relative.to_string_lossy();
                        let __harw_pattern = if __harw_relative.is_empty() {
                            ::std::string::String::from(#glob_lit)
                        } else {
                            ::std::format!("{}/{}", __harw_relative, #glob_lit)
                        };
                        ::harw_dod_readfs::glob::glob(__harw_scope, &__harw_pattern)
                            .map_err(#metrics_mod_ident::map_readfs_err)?
                    }
                };

                let mut __harw_samples: ::std::vec::Vec<::harw_dod_signals::HostSample> =
                    ::std::vec::Vec::new();
                for __harw_path in &__harw_paths {
                    let __harw_value = #reader_path(__harw_scope, __harw_path)
                        .map_err(#metrics_mod_ident::map_readfs_err)?;
                    __harw_samples.push(::harw_dod_signals::HostSample {
                        sensor: self.handle.id().clone(),
                        observed_at: now,
                        metric: ::std::borrow::Cow::Borrowed(#metric_lit),
                        value: __harw_value as f64,
                    });
                }

                ::std::result::Result::Ok(::harw_dod_signals::SensorReading {
                    samples: __harw_samples,
                    events: ::std::vec::Vec::new(),
                })
            }
        }

        #[allow(non_snake_case)]
        mod #metrics_mod_ident {
            ::harw_macros::metrics! {
                /// Wie oft dieser Sensor abgerufen wurde.
                POLLS_TOTAL: counter, unit = count, labels = [], cardinality = single,
                    name = #polls_name;
                /// Wie oft ein Abruf scheiterte, aufgeschlüsselt nach Fehlerart.
                ERRORS_TOTAL: counter, unit = count, labels = ["kind"], cardinality = bounded(5),
                    name = #errors_name;
            }

            static POLLS_COUNT: ::std::sync::atomic::AtomicU64 =
                ::std::sync::atomic::AtomicU64::new(0);
            static ERRORS_COUNT: ::std::sync::atomic::AtomicU64 =
                ::std::sync::atomic::AtomicU64::new(0);

            /// Erhöht die Zähler dieses Sensors und meldet sie an `sink`.
            pub(super) fn record_poll(
                sink: &dyn ::harw_observe::TelemetrySink,
                outcome: &::std::result::Result<
                    ::harw_dod_signals::SensorReading,
                    ::harw_dod_cap::SensorError,
                >,
            ) {
                let polls = POLLS_COUNT.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed) + 1;
                sink.record(&POLLS_TOTAL, ::harw_observe::MetricValue::Count(polls), &[]);

                if let ::std::result::Result::Err(err) = outcome {
                    let errors =
                        ERRORS_COUNT.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed) + 1;
                    sink.record(
                        &ERRORS_TOTAL,
                        ::harw_observe::MetricValue::Count(errors),
                        &[(
                            ::harw_observe::FieldName::from_static_unchecked("kind"),
                            ::harw_observe::FieldValue::Str(__harw_error_kind(err)),
                        )],
                    );
                }
            }

            /// Ordnet jeder `SensorError`-Variante ein stabiles Label zu.
            ///
            /// `ToolFault` (Korrektur K79: „unser Werkzeug kann die Quelle
            /// nicht verarbeiten", nicht die Quelle selbst ist fehlerhaft)
            /// bekommt das Label `"tool_fault"` — kleingeschrieben und mit
            /// Unterstrichen, wie die vier vorhandenen Label-Werte.
            fn __harw_error_kind(err: &::harw_dod_cap::SensorError) -> &'static str {
                match err {
                    ::harw_dod_cap::SensorError::OutsideScope => "outside_scope",
                    ::harw_dod_cap::SensorError::SourceUnavailable => "source_unavailable",
                    ::harw_dod_cap::SensorError::MalformedSource => "malformed_source",
                    ::harw_dod_cap::SensorError::ToolFault => "tool_fault",
                    ::harw_dod_cap::SensorError::Io(_) => "io",
                }
            }

            /// Bildet einen `harw_dod_readfs::ReadFsError` auf `SensorError` ab.
            ///
            /// `ReadFsError::Scope` reicht den bereits inhaltsfreien
            /// `SensorError` unverändert durch. Die vier übrigen Varianten
            /// (`TooLarge`, `GlobPatternAbsolute`, `GlobPatternTraversal`,
            /// `GlobLimitExceeded`) beschreiben eine Quelle, die nicht die
            /// erwartete Form/Größe hat, bzw. ein Glob-Muster, das nicht wie
            /// erwartet aufgelöst werden konnte oder eine der
            /// `harw_dod_readfs::glob`-Grenzen (Musterlänge, Kandidatenzahl)
            /// überschritten hat — alle vier bilden auf `MalformedSource` ab,
            /// ohne den mitgeführten Text (Muster oder Grenze) zu übernehmen.
            /// `GlobLimitExceeded` neu seit der `harw-dod-cap`/`harw-dod-readfs`-
            /// Korrektur C-SCOPE (Register `x-findings-register-w1-w3.md`,
            /// Alias-Wurzeln/Glob-Grenzen für sysfs-Klassenpfade): ohne diesen
            /// Arm kompiliert jeder `#[derive(SensorSource)]`-Sensor nicht
            /// mehr, sobald `ReadFsError` diese Variante trägt (erschöpfendes
            /// `match`, siehe `harw-dod-cap`-Vertrag Regel 7).
            pub(super) fn map_readfs_err(
                err: ::harw_dod_readfs::ReadFsError,
            ) -> ::harw_dod_cap::SensorError {
                match err {
                    ::harw_dod_readfs::ReadFsError::Scope(inner) => inner,
                    ::harw_dod_readfs::ReadFsError::TooLarge { .. }
                    | ::harw_dod_readfs::ReadFsError::GlobPatternAbsolute { .. }
                    | ::harw_dod_readfs::ReadFsError::GlobPatternTraversal { .. }
                    | ::harw_dod_readfs::ReadFsError::GlobLimitExceeded { .. } => {
                        ::harw_dod_cap::SensorError::MalformedSource
                    }
                }
            }
        }
    };

    let from_impl = match &args.error_path {
        Some(path) => quote! {
            impl ::std::convert::From<#path> for ::harw_dod_cap::SensorError {
                fn from(err: #path) -> Self {
                    err.into_sensor_error()
                }
            }
        },
        None => TokenStream::new(),
    };

    Ok(quote! {
        #sensor_impl
        #from_impl
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn validate_capability_accepts_all_fourteen_variants() {
        for name in CAPABILITY_VARIANTS {
            let ident = Ident::new(name, proc_macro2::Span::call_site());
            assert!(validate_capability(&ident).is_ok(), "must accept {name}");
        }
    }

    #[test]
    fn validate_capability_rejects_unknown_name() -> TestResult {
        let ident = Ident::new("Bogus", proc_macro2::Span::call_site());
        let Err(err) = validate_capability(&ident) else {
            return Err(TestError::Unexpected(
                "unknown capability must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannte capability"));
        Ok(())
    }

    fn valid_input() -> DeriveInput {
        syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "harw_dod_thermal_")]
            struct ThermalSensor {
                handle: harw_dod_cap::SensorHandle<harw_dod_cap::Bound>,
                #[source(glob = "thermal_zone*/temp", parse = "i64", metric = "temperature_celsius")]
                zones: (),
            }
        }
    }

    #[test]
    fn expand_rejects_enum() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "x", metrics = "p_")]
            enum Foo { A, B }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected("enums must be rejected".to_owned()));
        };
        assert!(err.to_string().contains("structs"));
        Ok(())
    }

    #[test]
    fn expand_rejects_tuple_struct() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "x", metrics = "p_")]
            struct Foo(u8);
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "tuple structs must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("benannten Feldern"));
        Ok(())
    }

    #[test]
    fn expand_rejects_two_capabilities() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(
                capability = ReadSysfsThermal,
                id = "thermal",
                metrics = "harw_dod_thermal_",
                capability = ReadProcStat,
            )]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "a/b", parse = "i64", metric = "m")]
                zones: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "two capabilities must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("`capability` doppelt angegeben"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_capability() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = Bogus, id = "thermal", metrics = "harw_dod_thermal_")]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "a/b", parse = "i64", metric = "m")]
                zones: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "unknown capability must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannte capability"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_sensor_key() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "p_", bogus = "x")]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "a/b", parse = "i64", metric = "m")]
                zones: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "unknown sensor key must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannter sensor-Schlüssel"));
        Ok(())
    }

    #[test]
    fn expand_rejects_missing_handle_field() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "p_")]
            struct ThermalSensor {
                #[source(glob = "a/b", parse = "i64", metric = "m")]
                zones: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "missing handle field must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("Feld `handle:"));
        Ok(())
    }

    #[test]
    fn expand_rejects_missing_source_field() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "p_")]
            struct ThermalSensor {
                handle: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "missing source field must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("keines gefunden"));
        Ok(())
    }

    #[test]
    fn expand_rejects_two_source_fields() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "p_")]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "a/b", parse = "i64", metric = "m")]
                a: (),
                #[source(glob = "c/d", parse = "u64", metric = "n")]
                b: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "two source fields must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("ist zwei Sensoren"));
        Ok(())
    }

    #[test]
    fn expand_rejects_empty_glob() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "p_")]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "", parse = "i64", metric = "m")]
                zones: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "empty glob must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("Glob-Muster darf nicht leer sein"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_parse_mode() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "p_")]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "a/b", parse = "f64", metric = "m")]
                zones: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "unknown parse mode must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannter parse-Modus"));
        Ok(())
    }

    #[test]
    fn expand_rejects_unknown_source_key() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsThermal, id = "thermal", metrics = "p_")]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "a/b", parse = "i64", metric = "m", bogus = "x")]
                zones: (),
            }
        };
        let Err(err) = expand_sensor_source(&input) else {
            return Err(TestError::Unexpected(
                "unknown source key must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("unbekannter source-Schlüssel"));
        Ok(())
    }

    #[test]
    fn expand_accepts_valid_sensor_and_generates_expected_items() -> TestResult {
        let tokens = expand_sensor_source(&valid_input())
            .map_err(ctx("valid sensor must expand"))?
            .to_string();

        assert!(tokens.contains("impl ThermalSensor"));
        assert!(tokens.contains("impl :: harw_dod_signals :: Sensor for ThermalSensor"));
        assert!(tokens.contains("CAPABILITY"));
        assert!(tokens.contains("Capability :: ReadSysfsThermal"));
        assert!(tokens.contains("SENSOR_ID"));
        assert!(tokens.contains("\"thermal\""));
        assert!(tokens.contains("record_poll_metrics"));
        assert!(tokens.contains(":: harw_dod_readfs :: glob :: glob"));
        assert!(tokens.contains(":: harw_dod_readfs :: parse_i64"));
        assert!(tokens.contains("\"harw_dod_thermal_polls_total\""));
        assert!(tokens.contains("\"harw_dod_thermal_errors_total\""));
        assert!(tokens.contains("\"temperature_celsius\""));
        assert!(!tokens.contains("std :: fs"));
        assert!(!tokens.contains("SystemTime"));
        assert!(!tokens.contains("Timestamp :: now"));
        // Ohne `#[sensor(error = ...)]` darf keine `From`-Brücke entstehen.
        assert!(!tokens.contains("into_sensor_error"));
        Ok(())
    }

    #[test]
    fn expand_with_error_attribute_emits_from_bridge() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(
                capability = ReadSysfsThermal,
                id = "thermal",
                metrics = "harw_dod_thermal_",
                error = "crate::error::ThermalError",
            )]
            struct ThermalSensor {
                handle: (),
                #[source(glob = "a/b", parse = "i64", metric = "m")]
                zones: (),
            }
        };
        let tokens = expand_sensor_source(&input)
            .map_err(ctx("sensor with error path must expand"))?
            .to_string();

        assert!(tokens.contains(
            "impl :: std :: convert :: From < crate :: error :: ThermalError > \
             for :: harw_dod_cap :: SensorError"
        ));
        assert!(tokens.contains("into_sensor_error"));
        Ok(())
    }

    #[test]
    fn expand_uses_u64_reader_when_requested() -> TestResult {
        let input: DeriveInput = syn::parse_quote! {
            #[sensor(capability = ReadSysfsBlock, id = "block", metrics = "harw_dod_block_")]
            struct BlockSensor {
                handle: (),
                #[source(glob = "sys/block/*/stat", parse = "u64", metric = "sectors_read")]
                devices: (),
            }
        };
        let tokens = expand_sensor_source(&input)
            .map_err(ctx("u64 sensor must expand"))?
            .to_string();
        assert!(tokens.contains(":: harw_dod_readfs :: parse_u64"));
        assert!(!tokens.contains(":: harw_dod_readfs :: parse_i64"));
        Ok(())
    }
}
