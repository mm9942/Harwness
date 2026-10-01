//! Proc-macros for the harwness project.
//!
//! The flagship macro is [`macro@HarwError`], a dependency-free replacement for
//! `thiserror`. It generates `Display`, `std::error::Error` and `From` impls
//! from an annotated enum, plus a companion `Result` type alias.
//!
//! The `Tool` and `tool` macros provide tool metadata and execution bindings;
//! `agent` currently validates declarative metadata and passes the item through
//! until an agent runtime binding exists.
//!
//! [`macro@FromRawArgs`] derives [`harw_operations::FromRawArgs`] — sowohl für
//! Argument-Structs als auch für Subcommand-Enums mit `#[raw(subcommand)]`.
//! Achtung: `#[raw(nth = N)]` ist im Struct-Pfad 1-basiert auf dem vollen
//! Token-Slice, im Enum-Pfad dagegen 0-basiert relativ zu den Tokens **nach**
//! dem Subcommand — siehe Spec-Abschnitt "FromRawArgs-Derive-Makro".
//!
//! [`macro@field`] und [`macro@metrics`] sind funktionsartige Makros für den
//! Telemetrie-Vertrag aus `harw-observe` (Vertragsabschnitt A in
//! `docs/design/build-history.md`): validierte Feldnamen bzw. registrierte
//! Metrikschlüssel, beide zur Compile-Zeit geprüft, damit ein ungültiger
//! Name oder ein falscher Metrikname den Build bricht statt erst einen
//! Golden-Test oder eine Laufzeitprüfung drei Wellen später.
//! [`macro@Redact`] leitet `harw_observe::Redact` ab, mit
//! `Redacted::Omitted` als Voreinstellung für jedes Feld ohne
//! `#[redact(...)]`-Attribut. `harw-macros` selbst hängt **nicht** von
//! `harw-observe` ab — die erzeugten Pfade sind rein textuell
//! (`::harw_observe::...`); umgekehrt hängt `harw-observe` auf `harw-macros`.
//!
//! [`macro@traced`] (AW1-02) erzeugt einen `tracing`-Span um eine Funktion.
//! `async fn` expandiert zu `.instrument(span).await`, niemals zu
//! `span.enter()` (Begründung in [`traced`]-Moduldoc, Punkt 1); Feldwerte
//! laufen über `::harw_observe::Redact`, nie über `Debug`, und werden erst
//! nach der Aktivierungsprüfung berechnet. `harw-macros` hängt auch hier
//! nicht produktiv von `tracing` oder `harw-observe` ab — beide Pfade sind
//! rein textuell (`::tracing::...`, `::harw_observe::Redact`).
//!
//! [`macro@SensorSource`] (AW2-06, dritter von vier Makro-Knoten nach AW0-02
//! und AW1-02) leitet `harw_dod_signals::Sensor` für eine glob-adressierte
//! sysfs-/procfs-Quelle ab: `poll()` liest ausschließlich über
//! `harw-dod-readfs`, reicht die injizierte Zeit unverändert durch und
//! erzwingt genau eine `harw_dod_cap::Capability` je Sensor. `harw-macros`
//! hängt auch hier nicht produktiv von `harw-dod-cap`, `harw-dod-signals`
//! oder `harw-dod-readfs` ab — alle erzeugten Pfade sind rein textuell
//! (`::harw_dod_cap::...`, `::harw_dod_signals::...`,
//! `::harw_dod_readfs::...`).
//!
//! [`macro@warden_actions`] (AW5-01, vierter und letzter Makro-Knoten nach
//! AW0-02, AW1-02 und AW2-06) erzeugt aus einer einzigen Deklaration fünf
//! zusammengehörige Erzeugnisse für eine Warden-Durchsetzungsaktion: das
//! Wire-Enum, die unautorisierte Vorschlagsform, ein striktes Tool-Schema,
//! den Audit-Typ und die Zulässigkeitsmatrix. Zwei Angaben pro Aktion sind
//! ohne Fallback Pflicht — `admissible_from` und `audit` —, und jeder
//! Feldtyp muss aus einer geschlossenen Positivliste stammen (`String`
//! ausdrücklich ausgeschlossen), damit ein Modell keine Aktion mit freiem
//! Text vorschlagen kann. `harw-macros` hängt auch hier nicht produktiv von
//! `harw-tools`, `harw-types` oder dem später gebauten
//! `harw-dod-warden-proto` ab — alle erzeugten Pfade sind rein textuell.
//!
//! # Modulaufbau
//!
//! Proc-Macro-Crates dürfen `#[proc_macro*]`-Funktionen nur aus dem Crate-Root
//! exportieren; dieses Modul enthält deshalb ausschließlich die dünnen
//! Einstiegspunkte, die an die eigentliche Expansionslogik in den
//! Untermodulen delegieren:
//! - [`error`][]: `#[derive(HarwError)]`-Expansion.
//! - [`tool`][]: `#[derive(Tool)]`- und `#[tool]`-Expansion.
//! - [`raw_args`][]: `#[derive(FromRawArgs)]`-Expansion.
//! - [`agent`][]: `#[agent]`-Expansion.
//! - [`operation`][]: `#[operation]`-Expansion.
//! - [`schema`][]: gemeinsame JSON-Schema-Inferenz (von [`tool`] genutzt).
//! - [`util`][]: gemeinsame Namenshelfer (z. B. `pascal_case`).
//! - [`field`][]: `field!`-Expansion (validierte Feldnamen für `harw-observe`).
//! - [`metrics`][]: `metrics!`-Expansion (Metrikschlüssel-Registrierung für
//!   `harw-observe`).
//! - [`redact`][]: `#[derive(Redact)]`-Expansion.
//! - [`traced`][]: `#[traced]`-Expansion (`tracing`-Span für Funktionen).
//! - [`sensor_source`][]: `#[derive(SensorSource)]`-Expansion
//!   (`harw_dod_signals::Sensor` für glob-adressierte Sensor-Quellen).
//! - [`warden_actions`][]: `warden_actions!`-Expansion (Wire-Enum,
//!   Vorschlagsform, Tool-Schema, Audit-Typ und Zulässigkeitsmatrix für den
//!   Warden-Durchsetzer).

use proc_macro::TokenStream;
use syn::{DeriveInput, ItemFn, parse_macro_input};

mod agent;
mod contributor;
mod error;
mod field;
mod id;
mod kebab_enum;
mod metrics;
mod op_args;
mod operation;
mod raw_args;
mod redact;
mod schema;
mod sensor_source;
mod tool;
mod traced;
mod util;
mod warden_actions;

#[cfg(test)]
mod test_support;

/// Derive `Display`, `std::error::Error` and `From` for an error enum.
///
/// Per-variant attributes:
/// - `#[msg("...")]` controls the `Display` output. `{0}`, `{1}`, ... refer to
///   tuple fields, `{name}` refers to named fields.
/// - `#[from]` generates a `From<Inner>` impl and wires `source()` to the inner
///   error. Only valid on single-field tuple variants.
///
/// When the enum name ends in `Error`, a `pub type <Prefix>Result<T> =
/// Result<T, <Enum>>;` alias is also emitted (e.g. `CoreError` -> `CoreResult`).
#[proc_macro_derive(HarwError, attributes(msg, from))]
pub fn derive_harw_error(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match error::expand_harw_error(&input) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Derive a `tool_spec()` constructor that builds a `::harw_tools::ToolSpec`
/// from an argument struct.
///
/// Struct-level attribute:
/// - `#[tool(name = "...", description = "...")]` — the tool name (defaults to
///   the struct identifier) and description (defaults to empty).
///
/// Per-field:
/// - `///` doc comments become the field's JSON-Schema `description`.
/// - `#[tool(default = ...)]` emits the JSON-compatible literal as the field's
///   JSON-Schema `default` and marks the field as optional (omitted from
///   `required`).
///
/// Field Rust types map onto JSON-Schema types: `String`/`&str` → string,
/// `u*`/`i*`/`usize`/`isize` → integer, `f32`/`f64` → number, `bool` → boolean,
/// `Vec<T>` → array (with item schema), `Option<T>` → schema of `T` but not
/// required. Unsupported types are rejected at macro expansion time.
#[proc_macro_derive(Tool, attributes(tool))]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match tool::expand_tool(&input) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Turn an `async fn` into a `ToolExecutor` implementation, including the
/// sandbox prologues the tool needs.
///
/// ```ignore
/// #[tool(
///     name = "fs.glob",
///     description = "Matches workspace files against a glob pattern",
///     permission = "read_workspace",
///     parallel_safe,
/// )]
/// async fn fs_glob(
///     context: &ToolExecutionContext,
///     args: GlobArgs,
/// ) -> Result<ToolOutput, ToolsError> { ... }
/// ```
///
/// # Attribute grammar
///
/// | Key | Form | Default | Meaning |
/// |---|---|---|---|
/// | `name` | `= "..."` | the function name | tool name exposed to the model |
/// | `description` | `= "..."` | `""` | tool description |
/// | `permission` | `= "..."` | none | sandbox permission checked before anything else |
/// | `host_from` | `= "<field>"` | none | args field holding the URL whose host is checked |
/// | `parallel_safe` | flag or `= true`/`= false` | `false` | whether concurrent calls are safe |
/// | `state` | `= Type` | none | the executor carries a `Type`; the fn takes `&Type` as an extra first argument |
/// | `schema_from` | `= path` | none | `path()` returns the `ToolSpec` (externally built schema) instead of `Args::tool_spec()` |
///
/// Accepted `permission` values are `"read_workspace"`, `"write_workspace"`,
/// `"execute_process"`, `"network_access"`, `"read_secrets"`,
/// `"manage_plugins"` and `"read_cargo_registry"` — the variants of
/// `harw_authority::Permission`. Any other value is a compile error listing the
/// allowed set rather than a silent fallback onto a different permission; a
/// typo must never resolve to a *different* (possibly weaker) permission.
///
/// # Stateful tools and external schemas
///
/// ```ignore
/// #[tool(
///     name = "palace.search",
///     permission = "read_workspace",
///     state = Arc<KnowledgeStore>,
///     schema_from = search_spec,
///     parallel_safe,
/// )]
/// async fn palace_search(
///     store: &Arc<KnowledgeStore>,
///     context: &ToolExecutionContext,
///     args: SearchArgs,
/// ) -> Result<ToolOutput, ToolsError> { ... }
/// ```
///
/// With `state = Type` the wrapper is `struct PalaceSearchTool { state: Type }`
/// with `PalaceSearchTool::new(state)` (no `Default`, no `Copy`; `Type` must be
/// `Debug + Clone`) and the function receives `&Type` as its first argument. A
/// `tool_provider!` with `state` builds such tools from a constructor
/// expression. With `schema_from = path`, `spec()` calls `path()` (which must
/// return `::harw_tools::ToolSpec`) and the args type does not need
/// `#[derive(Tool)]`; name and description are still overwritten from
/// `NAME` / `DESCRIPTION`.
///
/// # What is generated
///
/// 1. The original `async fn`, unchanged.
/// 2. A wrapper struct (`FsGlobTool`) — a unit struct deriving `Debug`,
///    `Clone`, `Copy` and `Default`, or (with `state`) a one-field struct with
///    `new(state)`.
/// 3. Associated consts `NAME`, `DESCRIPTION`, `PARALLEL_SAFE: bool` and
///    `PERMISSION: Option<::harw_tools::Permission>`. `PERMISSION` is the
///    auditable declaration; the prologue below is the actual enforcement.
/// 4. `pub fn spec() -> ::harw_tools::ToolSpec`, built from the *second*
///    argument's type (which must derive [`macro@Tool`]) with `name` and
///    `description` overwritten from `NAME` / `DESCRIPTION`, so a provider can
///    never advertise a name its `executor(name)` does not answer to.
/// 5. `impl ::harw_tools::ToolExecutor`, whose `execute` body runs, in order:
///    the `require_permission` guard (when `permission` is set), the
///    `serde_json::from_value` deserialisation, the `require_host_access` guard
///    (when `host_from` is set), and finally the original function.
///
/// # Security properties
///
/// - **Permission before deserialisation.** Model-controlled arguments are not
///   parsed until the sandbox has authorised the call.
/// - **Fail-closed host extraction.** When `host_from_url` cannot extract a
///   host, the generated code returns a `ToolOutput::Error` instead of passing
///   an empty host into the network-scope check.
/// - **`parallel_safe` defaults to `false`.** Side effects are presumed
///   non-commutative unless the tool explicitly says otherwise.
/// - **`host_from` requires `permission = "network_access"`**, otherwise the
///   host check would run without any guarantee that network access is allowed
///   at all. Declaring one without the other is a compile error.
///
/// # Path requirements on the consumer crate
///
/// The generated tokens reference `::harw_tools::{Permission, ToolCall,
/// ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName,
/// ToolOutput, ToolSpec, require_permission, require_host_access,
/// host_from_url}` and `::serde_json`. A crate using `#[tool]` must therefore
/// depend on `harw-tools` and `serde_json`; it does **not** need a direct
/// `harw-sandbox` dependency, because `Permission` is re-exported by
/// `harw-tools`.
///
/// # Compile-time diagnostics
///
/// - not an `async fn`, wrong argument count, or a non-reference first argument;
/// - unknown attribute key;
/// - unknown `permission` value (the message lists the allowed values);
/// - `host_from` without `permission = "network_access"`;
/// - `host_from` whose value is not a valid field identifier.
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = match tool::parse_tool_attr(attr.into()) {
        Ok(parsed) => parsed,
        Err(err) => return err.to_compile_error().into(),
    };

    let func = parse_macro_input!(item as ItemFn);
    match tool::expand_tool_fn(func, parsed) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Leitet `harw_operations::FromRawArgs` für ein Argument-Struct oder ein
/// Subcommand-Enum ab.
///
/// Die gesamte Analyse- und Expansionslogik liegt in [`raw_args`][]; dieser
/// Einstiegspunkt parst nur den [`DeriveInput`] und delegiert an
/// `raw_args::expand_from_raw_args`.
///
/// # ⚠ Semantik-Unterschied bei `nth` — Struct vs. Enum
///
/// Der wichtigste Fallstrick des Makros: `nth` bezieht sich auf **verschiedene
/// Slices**, je nachdem ob es an einem Struct oder an einer Enum-Variante steht.
///
/// | Kontext | Bezugs-Slice | `nth = 0` | Bedeutung von `nth = N` |
/// |---|---|---|---|
/// | **Struct** | der volle `tokens`-Slice | **Compile-Fehler** (nutze `first`) | `tokens[N]`, N ≥ 1 |
/// | **Enum-Variante** | `tokens[1..]` (alles nach dem Subcommand) | **erlaubt und üblich** | `tokens[1 + N]` |
///
/// Im Enum-Pfad ist `nth` also **0-basiert relativ zum Rest**: `#[raw(nth = 0)]`
/// meint das Token **direkt nach** dem Subcommand. Bei `plan create my-id …`
/// liefert `#[raw(nth = 0)]` folglich `"my-id"`, nicht `"create"`.
///
/// # Struct-Form
///
/// Jedes benannte Feld **muss** genau eines dieser Attribute tragen, und jedes
/// Feld muss den Typ `Option<String>` haben:
///
/// - `#[raw(first)]` — erstes Token (oder `None`).
/// - `#[raw(join)]` — alle Tokens, mit Leerzeichen verbunden (oder `None`).
/// - `#[raw(nth = N)]` — N-tes Token, N ≥ 1 (für N = 0 `first` benutzen);
///   `None`, wenn der Slice zu kurz ist.
/// - `#[raw(join_from = N)]` — alle Tokens ab dem 0-basierten Index N, mit
///   Leerzeichen verbunden; `None`, wenn ab dort keine Tokens mehr da sind.
/// - `#[raw(required)]` — erstes Token; fehlt es, liefert `from_raw_args`
///   `Err(OpError::InvalidArguments)`. Höchstens ein Feld pro Struct.
///
/// Structs mit null benannten Feldern übersetzen problemlos. Tupel- und
/// Unit-Structs werden mit einem Compile-Fehler abgelehnt.
///
/// ```ignore
/// #[derive(FromRawArgs)]
/// pub struct SkillsArgs {
///     #[raw(first)] action: Option<String>,
///     #[raw(nth = 1)] name: Option<String>,
/// }
/// ```
///
/// # Enum-Form (Subcommands)
///
/// Trägt das Enum `#[raw(subcommand)]`, wählt das **erste** Token die Variante
/// und die **restlichen** Tokens werden über die Variantenfelder verteilt:
///
/// ```ignore
/// #[derive(FromRawArgs)]
/// #[raw(subcommand)]
/// pub enum PlanArgs {
///     /// `plan create <id> <goal…>`
///     Create {
///         #[raw(nth = 0)] id: Option<String>,
///         #[raw(join_from = 1)] goal: Option<String>,
///     },
///     /// `plan ready`
///     Ready,
///     /// `plan status <id> <status>`
///     Status {
///         #[raw(nth = 0)] id: Option<String>,
///         #[raw(nth = 1)] status: Option<String>,
///     },
///     /// `plan inspect` oder `plan ls`
///     #[raw(alias = "ls")]
///     Inspect,
/// }
/// ```
///
/// Regeln der Variantenauswahl:
///
/// - Akzeptiert werden der Variantenname in `kebab-case` **und** in
///   `snake_case` (`AddCriterion` matcht `add-criterion` und `add_criterion`);
///   der Vergleich ist case-insensitiv.
/// - `#[raw(alias = "…")]` an einer Variante ergänzt weitere Tokens und darf
///   mehrfach vorkommen. Aliase erscheinen nicht in den Usage-Meldungen.
/// - `#[raw(default_subcommand)]` an **genau einer** Variante greift, wenn gar
///   kein Token vorhanden ist (z. B. `Show`).
/// - **Unit-Varianten ignorieren überzählige Tokens bewusst** — `/plan ready
///   --verbose` liefert `PlanArgs::Ready` statt eines Fehlers.
/// - Fehlt das erste Token (und gibt es kein `default_subcommand`), entsteht
///   `OpError::InvalidArguments` mit der alphabetischen kebab-case-Liste aller
///   Subcommands. Ein unbekanntes erstes Token liefert dieselbe Liste, ergänzt
///   um den unbekannten Wert.
///
/// # Compile-Zeit-Diagnosen
///
/// Gemeinsam (Struct und Enum):
/// 1. Falscher Feldtyp (nicht `Option<String>`) → Fehler.
/// 2. Widersprüchliche `#[raw(...)]`-Attribute an einem Feld → Fehler.
/// 3. Feld ohne `#[raw(...)]`-Attribut → Fehler.
/// 4. Mehr als ein `#[raw(required)]` pro Struct bzw. pro Variante → Fehler.
/// 5. Unbekannter `raw`-Schlüssel → Fehler.
///
/// Nur Struct-Pfad:
/// 6. Tupel- oder Unit-Struct → Fehler.
/// 7. `nth = 0` (stattdessen `first` benutzen) → Fehler.
/// 8. `#[raw(subcommand)]` an einem Struct → Fehler.
///
/// Nur Enum-Pfad:
/// 9. Enum ohne `#[raw(subcommand)]` → Fehler.
/// 10. Enum ohne Varianten → Fehler.
/// 11. Tupel-Variante (nur Unit- und Struct-Varianten sind erlaubt) → Fehler.
/// 12. Zwei Varianten beanspruchen denselben Subcommand-Token → Fehler.
/// 13. Mehr als ein `#[raw(default_subcommand)]` → Fehler.
///
/// # Design-doc reference
/// Spec-Abschnitt "FromRawArgs-Derive-Makro" im `harw-macros`-Erweiterungsbrief,
/// ergänzt um AP W1-26b (Subcommand-Enums, `join_from`).
#[proc_macro_derive(FromRawArgs, attributes(raw))]
pub fn derive_from_raw_args(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match raw_args::expand_from_raw_args(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Validate an agent declaration and pass the annotated function through.
///
/// The current runtime has no agent registration or execution trait for this
/// macro to implement.  Consequently, this macro deliberately emits the
/// original function unchanged after validating its declaration.
///
/// # Supported arguments
///
/// - `name = "..."`
/// - `role = "..."`
/// - `description = "..."`
///
/// Each argument may occur at most once.  The annotated item must be a
/// function; no runtime behavior is generated.
#[proc_macro_attribute]
pub fn agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match agent::parse_agent_args(attr.into()) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };
    let item = match syn::parse::<syn::Item>(item) {
        Ok(item) => item,
        Err(err) => return err.to_compile_error().into(),
    };

    match agent::expand_agent(item, args) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Transform an `async fn` into a unit struct + `impl ::harw_operations::Operation`.
///
/// # Syntax
///
/// ```ignore
/// #[operation(
///     name = "status",
///     summary = "Zeigt den Session-Status.",
///     domain = "misc",          // session | agents | execution | catalog_config | knowledge | identity | network | security | crypto | misc
///     permission = "observer",  // observer | operator | maintainer | owner
///     command(path = "/status", visibility = "channel_parity"),   // optional
///     // command(...) kennt zusätzlich `busy = "immediate" | "staged" | "deferred"`
///     // und `busy_subcommands = "show=immediate, switch=staged, -=immediate"`
///     // (`-` = bare Form; überschreibt `Operation::busy_subcommands`).
///     model_tool(readonly, approval = "none"),                    // optional; approval: none | always
///     agent_tool(child = "researcher", authority = "reduce_to_read_only", budget = "8k"), // optional
/// )]
/// async fn status(ctx: &::harw_operations::OpContext, args: StatusArgs)
///     -> Result<::harw_operations::OpOutput, ::harw_operations::OpError>
/// { ... }
/// ```
///
/// # What is generated
///
/// 1. The original `async fn` is re-emitted unchanged.
/// 2. A `pub struct <FnNamePascalCase>Operation;` unit struct is emitted.
/// 3. An `impl ::harw_operations::Operation for <Struct>` is emitted with:
///    - `fn meta(&self) -> &::harw_operations::OperationMeta`: returns a reference to a
///      static [`OperationMeta`] initialised via [`std::sync::OnceLock`]. The `surfaces`
///      field is built from the `command(...)`, `model_tool(...)`, and `agent_tool(...)`
///      sub-attributes that are present; absent sub-attributes produce no surface entry.
///    - `fn run<'a>(&'a self, ctx: &'a ::harw_operations::OpContext, input: ::harw_operations::OpInput) -> ::harw_operations::OpFuture<'a>`:
///      deserialises `input.json_args` into the second function argument type via
///      `::serde_json::from_value`. If `json_args` is `Null`, uses
///      `<ArgsType as ::core::default::Default>::default()` instead. On deserialisation
///      failure returns `Err(::harw_operations::OpError::InvalidArguments(...))`.
///
/// # Errors (compile-time)
///
/// - Missing required keys (`name`, `summary`, `domain`, `permission`) → `syn::Error`.
/// - Unknown top-level or nested attribute key → `syn::Error`.
/// - Unknown domain / permission / visibility / approval string → `syn::Error`.
/// - `agent_tool(...)` declared without all three of `child`, `authority`, `budget` → `syn::Error`.
/// - Not an `async fn` → `syn::Error`.
/// - Wrong argument count or types → `syn::Error`.
///
/// # Design-doc reference
/// Spec section: "API-Design des Makros" in the `harw-macros` extension brief.
#[proc_macro_attribute]
pub fn operation(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match operation::parse_operation_args(attr.into()) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };
    let func = parse_macro_input!(item as ItemFn);
    match operation::expand_operation(func, args) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

// ── Neue Ableitungen und Contributor-Attribute (W1-26d/e/f) ──────────────────

/// Leitet `harw_operations::OpArgsSchema` für einen Argument-Typ ab.
///
/// Erzeugt aus den Feldern (Struct) bzw. aus `action` + vereinigten Varianten-
/// feldern (Subcommand-Enum) ein **geschlossenes** JSON-Schema. Damit entfällt
/// der hartcodierte Schema-`match` für jede neue Model-Tool-Operation.
///
/// # Errors
/// - Tupel-/Unit-Struct oder Enum-Variante mit Tupelfeldern → `syn::Error`.
/// - Nicht unterstützter Feldtyp → `syn::Error`.
#[proc_macro_derive(OpArgs, attributes(raw, tool))]
pub fn derive_op_args(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match op_args::expand_op_args(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Leitet den Standard-Satz an ID-Newtype-Methoden ab (`try_new`, `as_str`,
/// `Display`, `FromStr`, `AsRef<str>`, `Borrow<str>`, `PartialEq<str>`).
///
/// Anwendbar auf `pub struct Foo(String);`. Der Fehlertyp wird über
/// `#[harw_id(error = "…", ctor = "…")]` konfiguriert; `#[harw_id(infallible)]`
/// erzeugt zusätzlich einen unvalidierten Kompatibilitätskonstruktor `new`.
///
/// # Errors
/// - Kein Tuple-Struct mit genau einem `String`-Feld → `syn::Error`.
/// - Unbekannter `harw_id`-Schlüssel → `syn::Error`.
#[proc_macro_derive(HarwId, attributes(harw_id))]
pub fn derive_harw_id(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match id::expand_harw_id(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Leitet `ALL`, `as_str`, `Display` und `FromStr` in kebab-case (oder mit
/// `#[kebab_enum(case = "snake")]` in snake_case) für ein Enum aus reinen
/// Unit-Varianten ab (`FromStr` akzeptiert kebab- und snake_case,
/// case-insensitiv). Optional: `parse_option` (`parse -> Option<Self>`),
/// `no_from_str`, `no_all`.
///
/// # Errors
/// - Nicht-Unit-Variante → `syn::Error`.
/// - Unbekannter `kebab_enum`-Schlüssel → `syn::Error`.
#[proc_macro_derive(KebabEnum, attributes(kebab_enum))]
pub fn derive_kebab_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match kebab_enum::expand_kebab_enum(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Erzeugt aus einer `async fn` einen `harw_extension_api::ContextProvider`.
///
/// Ersetzt das wiederkehrende `Box::pin(async move { … })`-Boilerplate. Der
/// Strukturname wird aus dem Funktionsnamen abgeleitet und kann über
/// `#[context_provider(struct_name = "…")]` überschrieben werden.
///
/// # Errors
/// - Keine `async fn`, falsche Parameteranzahl oder erster Parameter ohne
///   Referenztyp → `syn::Error`.
#[proc_macro_attribute]
pub fn context_provider(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match contributor::parse_context_provider_args(attr.into()) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };
    let func = parse_macro_input!(item as ItemFn);
    match contributor::expand_context_provider(func, args) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Erzeugt aus einer `async fn` einen `harw_extension_api::InstructionsProvider`.
///
/// # Errors
/// Siehe [`macro@context_provider`].
#[proc_macro_attribute]
pub fn instructions_provider(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match contributor::parse_instructions_provider_args(attr.into()) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };
    let func = parse_macro_input!(item as ItemFn);
    match contributor::expand_instructions_provider(func, args) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

// ── Telemetrie-Vertrag (AW0-02): `field!`, `metrics!`, `#[derive(Redact)]` ──

/// Baut einen zur Compile-Zeit geprüften `::harw_observe::FieldName`.
///
/// # Description
/// Erwartet genau ein String-Literal und expandiert zu
/// `::harw_observe::FieldName::from_static_unchecked(<literal>)`. `FieldName`
/// hat bewusst keinen öffentlichen validierenden Konstruktor — dieses Makro
/// ist der einzige vorgesehene Weg, einen Feldnamen zu erzeugen, damit ein
/// ungültiger Name niemals zur Laufzeit entstehen kann.
///
/// # Compile-Fehler bei
/// - leerem Feldnamen,
/// - einem Zeichen außerhalb `[a-z0-9_.]` (insbesondere Großbuchstaben),
/// - einem führenden oder abschließenden `.`,
/// - zwei aufeinanderfolgenden Punkten (`..`),
/// - einem Argument, das kein einzelnes String-Literal ist.
///
/// # Examples
/// ```ignore
/// let f = harw_macros::field!("child.admitted");
/// assert_eq!(f.as_str(), "child.admitted");
/// ```
#[proc_macro]
pub fn field(input: TokenStream) -> TokenStream {
    match field::expand_field(input.into()) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Deklariert einen oder mehrere `::harw_observe::MetricKey`s als `pub const`.
///
/// # Description
/// Erwartet eine `;`-getrennte Liste von Einträgen der Form
/// `NAME: kind, unit = ..., labels = [...], cardinality = ..., name = "...";`
/// (die vier Schlüssel nach `kind` müssen je genau einmal vorkommen,
/// Reihenfolge beliebig) und expandiert zu:
///
/// ```ignore
/// pub const NAME: ::harw_observe::MetricKey = ::harw_observe::MetricKey {
///     name: "...",
///     kind: ::harw_observe::MetricKind::...,
///     unit: ::harw_observe::Unit::...,
///     labels: &[::harw_observe::FieldName::from_static_unchecked("..."), ...],
///     cardinality: ::harw_observe::Cardinality::...,
/// };
/// // eine solche `const` je Eintrag, danach:
/// pub static ALL: &[&::harw_observe::MetricKey] = &[&NAME, ...];
/// ```
///
/// Doc-Kommentare vor einem Eintrag werden unverändert auf die erzeugte
/// `const` übertragen.
///
/// # Compile-Fehler bei
/// - Syntaxfehlern in der Deklaration (fehlender/unbekannter Schlüssel,
///   falscher Werttyp);
/// - zwei Einträgen mit demselben Konstantennamen innerhalb einer
///   Deklaration;
/// - einem `labels`-Eintrag, der kein gültiger Feldname ist (dieselbe Prüfung
///   wie [`macro@field`]);
/// - einem `counter`, dessen Name nicht auf `_total` endet;
/// - einer Einheit mit Basiseinheit (`bytes`, `seconds`, `tokens`, `celsius`),
///   deren Suffix (`_bytes`, `_seconds`, `_tokens`, `_celsius`) nicht als
///   Teilstring im Namen vorkommt;
/// - einem Namen, der Zeichen außerhalb `[a-z0-9_]` enthält.
///
/// # Examples
/// ```ignore
/// harw_macros::metrics! {
///     /// Wie viele Kinder aufgenommen wurden.
///     CHILD_ADMITTED: counter, unit = count, labels = ["clan", "role"], cardinality = bounded(64),
///         name = "harw_child_admitted_total";
///     /// Aktuelle Kontextkosten.
///     CONTEXT_COST: gauge, unit = tokens, labels = [], cardinality = single,
///         name = "harw_context_cost_tokens";
/// }
/// ```
#[proc_macro]
pub fn metrics(input: TokenStream) -> TokenStream {
    match metrics::expand_metrics(input.into()) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Leitet `::harw_observe::Redact` für eine Struct ab.
///
/// # Description
/// Erzeugt `impl ::harw_observe::Redact for <Struct>`, dessen `redact()` aus
/// den nicht ausgelassenen Feldern eine einzige Zeichenkette
/// `"<Struct> { feld: wert, ... }"` baut und als `Redacted::Shown(..)`
/// zurückgibt:
///
/// ```ignore
/// impl ::harw_observe::Redact for Foo {
///     fn redact(&self) -> ::harw_observe::Redacted { /* ... */ }
/// }
/// ```
///
/// Pro Feld gilt genau eine der drei Policies:
/// - **ohne Attribut** (Voreinstellung): das Feld erscheint nicht in der
///   Ausgabe. Das ist der Zweck der Voreinstellung: ein Feld, über das
///   niemand nachgedacht hat, landet nicht im Log.
/// - `#[redact(show)]`: das Feld erscheint über `Display`.
/// - `#[redact(hash)]`: das Feld erscheint als blake3-Hex-Hash seiner
///   `Display`-Darstellung, gekürzt auf 16 Zeichen.
///
/// # Path requirements on the consumer crate
///
/// Der erzeugte Code referenziert `::harw_observe::{Redact, Redacted}` sowie,
/// für `#[redact(hash)]`-Felder, `::blake3::hash`. Ein Crate, das
/// `#[derive(Redact)]` mit mindestens einem `#[redact(hash)]`-Feld nutzt,
/// muss deshalb selbst von `blake3` abhängen (Workspace-Version).
///
/// # Compile-Fehler bei
/// - einem Item, das keine Struct ist (Enum, Union);
/// - einer Tuple-Struct (nur benannte Felder oder eine Unit-Struct sind
///   erlaubt);
/// - unbekanntem Schlüssel in `#[redact(...)]` (erwartet: `show`, `hash`);
/// - widersprüchlichen `#[redact(...)]`-Attributen auf demselben Feld.
///
/// # Examples
/// ```ignore
/// #[derive(harw_macros::Redact)]
/// struct ChildEvent {
///     #[redact(show)]
///     clan: String,
///     #[redact(hash)]
///     token: String,
///     secret: String, // ohne Attribut -> erscheint nicht
/// }
/// ```
#[proc_macro_derive(Redact, attributes(redact))]
pub fn derive_redact(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match redact::expand_redact(&input) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

// ── Tracing-Span für Funktionen (AW1-02): `#[traced]` ───────────────────────

/// Erzeugt einen `tracing`-Span um den annotierten Funktionskörper.
///
/// # Description
///
/// ```ignore
/// #[harw_macros::traced(level = "debug", fields(child_id, clan))]
/// async fn admit_child(&self, child_id: &ChildId, clan: &ClanId) -> Result<(), Error> { … }
/// ```
///
/// `level = "..."` ist einer von `trace`, `debug`, `info`, `warn`, `error`
/// und optional (Voreinstellung `info`). `fields(a, b, ...)` benennt
/// Parameter der annotierten Funktion (nicht `self`) und ist ebenfalls
/// optional (Voreinstellung: keine Felder).
///
/// Für eine gewöhnliche `fn` wird der Körper in `span.enter()` ausgeführt:
///
/// ```ignore
/// fn admit_child(&self, child_id: &ChildId, clan: &ClanId) -> Result<(), Error> {
///     let __harw_traced_span = ::tracing::span!(
///         ::tracing::Level::DEBUG,
///         "admit_child",
///         child_id = ::tracing::field::Empty,
///         clan = ::tracing::field::Empty,
///     );
///     if !__harw_traced_span.is_disabled() {
///         // je Feld: `Redact::redact(..)` + `span.record(.., field::debug(..))`
///     }
///     let _harw_traced_guard = __harw_traced_span.enter();
///     { /* ursprünglicher Körper */ }
/// }
/// ```
///
/// Für eine `async fn` wird der Körper stattdessen in ein inneres
/// `async move { .. }` verschoben und über `.instrument(span).await`
/// ausgeführt:
///
/// ```ignore
/// async fn admit_child(&self, child_id: &ChildId, clan: &ClanId) -> Result<(), Error> {
///     let __harw_traced_span = ::tracing::span!( /* wie oben */ );
///     if !__harw_traced_span.is_disabled() { /* wie oben */ }
///     {
///         use ::tracing::Instrument as _;
///         async move { /* ursprünglicher Körper */ }.instrument(__harw_traced_span).await
///     }
/// }
/// ```
///
/// # Warum `.instrument`, niemals `span.enter()`, bei `async fn`
///
/// `span.enter()` liefert einen Guard, der über einen `.await`-Punkt hinweg
/// gehalten werden kann. Pausiert der Task dort, bleibt der Span aktiv; läuft
/// danach ein *anderer* Task auf demselben Executor-Thread weiter, erbt er
/// denselben Span. Das Ergebnis sind Spans, die Arbeit enthalten, die nie in
/// ihnen stattfand — und das ist an der Tracing-Ausgabe selbst nicht als
/// Fehler erkennbar, nur als falsche Zuordnung. `.instrument(span)` betritt
/// den Span dagegen nur für die Dauer jedes einzelnen `poll()`-Aufrufs und
/// verlässt ihn wieder, bevor der Task pausiert. Diese Umformung ändert nur
/// den Funktionskörper, nie die Signatur, und funktioniert deshalb
/// unabhängig von Generics, Lifetimes oder dem Empfängertyp.
///
/// # Compile-Fehler bei
///
/// - unbekanntem Attributschlüssel (nicht `level`/`fields`);
/// - `level`/`fields` doppelt angegeben;
/// - ungültigem `level`-Wert (nicht `trace`/`debug`/`info`/`warn`/`error`);
/// - einem `fields(...)`-Eintrag, der keinen Parameter der annotierten
///   Funktion benennt;
/// - einem in `fields(...)` genannten Argument, dessen Typ
///   `::harw_observe::Redact` nicht implementiert — der generierte
///   `(<arg>).redact()`-Aufruf schlägt dann mit „Methode nicht gefunden"
///   fehl; es gibt keinen stillen Rückfall auf `{:?}` des Rohwerts.
///
/// # Path requirements on the consumer crate
///
/// Der erzeugte Code referenziert immer `::tracing::{span, Level,
/// field::Empty}`; ein Crate, das `#[traced]` nutzt, muss deshalb von
/// `tracing` abhängen (Workspace-Version). Nutzt mindestens ein `fields(...)`
/// -Eintrag, referenziert der Code zusätzlich `::harw_observe::Redact` und
/// `::tracing::field::debug` — das Crate muss dann zusätzlich von
/// `harw-observe` abhängen (Workspace-Version), und jeder in `fields(...)`
/// genannte Argumenttyp muss `::harw_observe::Redact` implementieren (z. B.
/// über `#[derive(harw_macros::Redact)]`).
///
/// # Examples
///
/// ```ignore
/// #[harw_macros::traced(level = "info", fields(job_id))]
/// fn start_job(job_id: &JobId) -> Result<(), Error> { Ok(()) }
///
/// #[harw_macros::traced(level = "debug", fields(child_id, clan))]
/// async fn admit_child(&self, child_id: &ChildId, clan: &ClanId) -> Result<(), Error> {
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn traced(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match traced::parse_traced_args(attr.into()) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };
    let func = parse_macro_input!(item as ItemFn);
    match traced::expand_traced(func, args) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

// ── Sensor-Boilerplate (AW2-06): `#[derive(SensorSource)]` ──────────────────

/// Leitet `harw_dod_signals::Sensor` für eine glob-adressierte
/// sysfs-/procfs-Quelle ab.
///
/// # Description
///
/// ```ignore
/// #[derive(harw_macros::SensorSource)]
/// #[sensor(
///     capability = ReadSysfsThermal,
///     id = "thermal",
///     metrics = "harw_dod_thermal_",
/// )]
/// struct ThermalSensor {
///     handle: harw_dod_cap::SensorHandle<harw_dod_cap::Bound>,
///     #[source(
///         glob = "sys/class/thermal/thermal_zone*/temp",
///         parse = "i64",
///         metric = "temperature_celsius",
///     )]
///     zones: (),
/// }
/// ```
///
/// erzeugt wörtlich:
///
/// ```ignore
/// impl ThermalSensor {
///     pub const CAPABILITY: ::harw_dod_cap::Capability =
///         ::harw_dod_cap::Capability::ReadSysfsThermal;
///     pub const SENSOR_ID: &'static str = "thermal";
///
///     pub fn record_poll_metrics(
///         &self,
///         sink: &dyn ::harw_observe::TelemetrySink,
///         outcome: &::std::result::Result<
///             ::harw_dod_signals::SensorReading,
///             ::harw_dod_cap::SensorError,
///         >,
///     ) {
///         __harw_sensor_source_metrics_ThermalSensor::record_poll(sink, outcome);
///     }
/// }
///
/// impl ::harw_dod_signals::Sensor for ThermalSensor {
///     fn handle(&self) -> &::harw_dod_cap::SensorHandle<::harw_dod_cap::Bound> {
///         &self.handle
///     }
///
///     fn poll(
///         &self,
///         now: ::jiff::Timestamp,
///     ) -> ::std::result::Result<::harw_dod_signals::SensorReading, ::harw_dod_cap::SensorError> {
///         let _ = &self.zones;
///         let __harw_scope = self.handle.scope();
///         let __harw_paths = ::harw_dod_readfs::glob::glob(
///             __harw_scope,
///             "sys/class/thermal/thermal_zone*/temp",
///         )
///         .map_err(__harw_sensor_source_metrics_ThermalSensor::map_readfs_err)?;
///
///         let mut __harw_samples: ::std::vec::Vec<::harw_dod_signals::HostSample> =
///             ::std::vec::Vec::new();
///         for __harw_path in &__harw_paths {
///             let __harw_value = ::harw_dod_readfs::parse_i64(__harw_scope, __harw_path)
///                 .map_err(__harw_sensor_source_metrics_ThermalSensor::map_readfs_err)?;
///             __harw_samples.push(::harw_dod_signals::HostSample {
///                 sensor: self.handle.id().clone(),
///                 observed_at: now,
///                 metric: ::std::borrow::Cow::Borrowed("temperature_celsius"),
///                 value: __harw_value as f64,
///             });
///         }
///
///         ::std::result::Result::Ok(::harw_dod_signals::SensorReading {
///             samples: __harw_samples,
///             events: ::std::vec::Vec::new(),
///         })
///     }
/// }
///
/// #[allow(non_snake_case)]
/// mod __harw_sensor_source_metrics_ThermalSensor {
///     ::harw_macros::metrics! {
///         POLLS_TOTAL: counter, unit = count, labels = [], cardinality = single,
///             name = "harw_dod_thermal_polls_total";
///         ERRORS_TOTAL: counter, unit = count, labels = ["kind"], cardinality = bounded(4),
///             name = "harw_dod_thermal_errors_total";
///     }
///     // .. record_poll(sink, outcome), map_readfs_err(err) — siehe crate::sensor_source.
/// }
/// ```
///
/// Ist zusätzlich `#[sensor(error = "pfad::zu::LocalError")]` angegeben, entsteht
/// außerdem `impl ::std::convert::From<LocalError> for ::harw_dod_cap::SensorError`,
/// dessen `from` auf eine von Hand bereitzustellende Methode
/// `LocalError::into_sensor_error(self) -> ::harw_dod_cap::SensorError` delegiert
/// (siehe [`sensor_source::expand_sensor_source`] für die Begründung der
/// Waisenregel-Ausnahme, die dieses `impl` erst erlaubt).
///
/// # Attributgrammatik
///
/// Struct-Attribut `#[sensor(...)]`:
///
/// | Schlüssel | Form | Pflicht | Bedeutung |
/// |---|---|---|---|
/// | `capability` | `= Bezeichner` | ja, genau einmal | eine von 14 `harw_dod_cap::Capability`-Varianten |
/// | `id` | `= "..."` | ja | wird zu `Self::SENSOR_ID` |
/// | `metrics` | `= "..."` | ja | Namenspräfix der beiden erzeugten Zähler |
/// | `error` | `= "pfad::zu::Typ"` | nein | siehe „`From`-Brücke" oben |
///
/// Feld-Attribut `#[source(...)]`, an **genau einem** Feld:
///
/// | Schlüssel | Form | Pflicht | Bedeutung |
/// |---|---|---|---|
/// | `glob` | `= "..."` | ja, nicht leer | bereichsrelatives Muster für `harw_dod_readfs::glob::glob` |
/// | `parse` | `= "i64"` \| `"u64"` | ja | wählt `parse_i64`/`parse_u64` je Treffer |
/// | `metric` | `= "..."` | ja | `HostSample::metric` je Treffer |
///
/// Der Feldtyp des `#[source(...)]`-Feldes ist gleichgültig — das Makro
/// liest seinen Wert nie. Konvention: `()`.
///
/// # Die drei erzwungenen Regeln
///
/// 1. **Genau eine Capability je Sensor.** Zwei `capability = ...`-Angaben
///    in derselben Deklaration sind ein Compile-Fehler.
/// 2. **Kein `std::fs`.** Der erzeugte Code liest ausschließlich über
///    `::harw_dod_readfs::glob::glob`, `::harw_dod_readfs::parse_i64` bzw.
///    `::harw_dod_readfs::parse_u64`.
/// 3. **Injizierte Zeit.** `poll(&self, now: ::jiff::Timestamp)` reicht
///    `now` unverändert durch; die Systemuhr wird nie gelesen.
///
/// # Compile-Fehler bei
///
/// - dem annotierten Item ist kein Struct (Enum, Union);
/// - dem Struct fehlen benannte Felder (Tuple-/Unit-Struct);
/// - `capability`, `id`, `metrics` oder `error` fehlt oder ist doppelt
///   angegeben;
/// - `capability` benennt keine der 14 `harw_dod_cap::Capability`-Varianten;
/// - `error` ist kein gültiger Pfad;
/// - unbekannter Schlüssel in `#[sensor(...)]` oder `#[source(...)]`;
/// - kein Feld namens `handle`;
/// - kein Feld mit `#[source(...)]`, oder mehr als eines („ein Sensor mit
///   zwei Quellen ist zwei Sensoren");
/// - `glob` ist ein leeres String-Literal (ein absolutes Muster oder eines
///   mit `..` wird **nicht** hier, sondern erst zur Laufzeit von
///   `harw_dod_readfs::glob::glob` selbst abgelehnt — siehe
///   [`sensor_source`]-Moduldoku für die Begründung, warum dieses Makro
///   diese eine Prüfung bewusst nicht wiederholt);
/// - `parse` ist weder `"i64"` noch `"u64"`.
///
/// # Pfadanforderungen an die aufrufende Crate
///
/// Der erzeugte Code referenziert `::harw_dod_cap::{Capability, SensorHandle,
/// Bound, SensorError}`, `::harw_dod_signals::{Sensor, SensorReading,
/// HostSample}`, `::harw_dod_readfs::{glob::glob, parse_i64, parse_u64,
/// ReadFsError}`, `::harw_observe::{TelemetrySink, MetricValue, FieldName,
/// FieldValue}`, `::harw_macros::metrics!` und `::jiff::Timestamp`. Eine
/// Sensor-Crate, die `#[derive(SensorSource)]` nutzt, muss deshalb in ihrer
/// `Cargo.toml` von `harw-dod-cap`, `harw-dod-signals`, `harw-dod-readfs`,
/// `harw-observe`, `harw-macros` und `jiff` abhängen — alle Pfade sind rein
/// textuell, `harw-macros` selbst hängt auf keine dieser Crates produktiv.
///
/// # Examples
///
/// ```ignore
/// use harw_dod_cap::{Capability, SensorHandle, Bound};
///
/// #[derive(harw_macros::SensorSource)]
/// #[sensor(
///     capability = ReadSysfsThermal,
///     id = "thermal",
///     metrics = "harw_dod_thermal_",
/// )]
/// struct ThermalSensor {
///     handle: SensorHandle<Bound>,
///     #[source(
///         glob = "sys/class/thermal/thermal_zone*/temp",
///         parse = "i64",
///         metric = "temperature_celsius",
///     )]
///     zones: (),
/// }
/// ```
#[proc_macro_derive(SensorSource, attributes(sensor, source))]
pub fn derive_sensor_source(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match sensor_source::expand_sensor_source(&input) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

// ── Warden-Aktionstaxonomie (AW5-01): `warden_actions!` ─────────────────────

/// Erzeugt aus einer Aktionsdeklaration die fünf Erzeugnisse einer
/// Warden-Durchsetzungsaktion: Wire-Enum, Vorschlagsform, Tool-Schema,
/// Audit-Typ und Zulässigkeitsmatrix.
///
/// # Description
///
/// ```ignore
/// harw_macros::warden_actions! {
///     authorization_proof = harw_dod_warden_proto::AuthorizationProof;
///
///     /// Friert eine cgroup ein.
///     FreezeCgroup {
///         cgroup: harw_types::CgroupId,
///         admissible_from: [RuleTriggered, Escalated],
///         audit = "warden.freeze_cgroup",
///     }
///     /// Beendet einen Prozessbaum.
///     KillProcessTree {
///         cgroup: harw_types::CgroupId,
///         admissible_from: [Escalated],
///         audit = "warden.kill_process_tree",
///     }
/// }
/// ```
///
/// erzeugt wörtlich:
///
/// ```ignore
/// /// Wire-Enum: ... (siehe warden_actions.rs).
/// #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
/// #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
/// pub enum WardenAction {
///     /// Friert eine cgroup ein.
///     FreezeCgroup { cgroup: harw_types::CgroupId },
///     /// Beendet einen Prozessbaum.
///     KillProcessTree { cgroup: harw_types::CgroupId },
/// }
///
/// /// Was ein Agent vorschlagen kann, ohne Autorisierung. ...
/// #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
/// #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
/// pub enum ProposedAction {
///     /// Friert eine cgroup ein.
///     FreezeCgroup { cgroup: harw_types::CgroupId },
///     /// Beendet einen Prozessbaum.
///     KillProcessTree { cgroup: harw_types::CgroupId },
/// }
///
/// impl ::core::convert::From<ProposedAction> for WardenAction {
///     fn from(value: ProposedAction) -> Self {
///         match value {
///             ProposedAction::FreezeCgroup { cgroup } => WardenAction::FreezeCgroup { cgroup },
///             ProposedAction::KillProcessTree { cgroup } => WardenAction::KillProcessTree { cgroup },
///         }
///     }
/// }
///
/// /// Die tatsächliche Wire-Nachricht: eine Aktion plus der Autorisierungsbeleg.
/// #[derive(Debug, Clone, ::serde::Serialize, ::serde::Deserialize)]
/// #[serde(deny_unknown_fields)]
/// pub struct WardenActionRequest {
///     pub action: WardenAction,
///     pub proof: harw_dod_warden_proto::AuthorizationProof,
/// }
///
/// impl WardenActionRequest {
///     pub fn new(action: ProposedAction, proof: harw_dod_warden_proto::AuthorizationProof) -> Self {
///         Self { action: ::core::convert::From::from(action), proof }
///     }
/// }
///
/// /// Der Audit-Typ: was in die Audit-Kette geschrieben wird.
/// #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
/// pub struct WardenActionAudit {
///     pub audit_name: ::std::string::String,
///     pub action: WardenAction,
/// }
///
/// impl WardenActionAudit {
///     pub fn for_action(action: WardenAction) -> Self {
///         let audit_name: ::std::string::String = match &action {
///             WardenAction::FreezeCgroup { .. } => "warden.freeze_cgroup".to_string(),
///             WardenAction::KillProcessTree { .. } => "warden.kill_process_tree".to_string(),
///         };
///         Self { audit_name, action }
///     }
/// }
///
/// /// Zulässigkeitsmatrix-Skelett.
/// #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// pub enum EscalationStage {
///     RuleTriggered,
///     Escalated,
/// }
///
/// impl WardenAction {
///     pub fn is_admissible_from(&self, stage: EscalationStage) -> bool {
///         match self {
///             WardenAction::FreezeCgroup { .. } =>
///                 matches!(stage, EscalationStage::RuleTriggered | EscalationStage::Escalated),
///             WardenAction::KillProcessTree { .. } => matches!(stage, EscalationStage::Escalated),
///         }
///     }
/// }
///
/// impl ProposedAction {
///     pub fn tool_schema() -> ::harw_tools::ToolSpec {
///         // ein `any_of`-JSON-Schema mit einem `additionalProperties: false`-Objekt
///         // je Aktion, `strict: true` — siehe `warden_actions.rs` für die
///         // vollständige Konstruktion.
///         # unimplemented!()
///     }
/// }
/// ```
///
/// # Grammatik
///
/// `authorization_proof = <Pfad>;` steht genau einmal am Anfang, gefolgt von
/// mindestens einer Aktion. Jede Aktion ist `<Ident> { ... }`, wobei die
/// geschweiften Klammern beliebig viele Datenfelder (`name: Type,`) sowie
/// genau ein `admissible_from: [<Ident>, ...],` und genau ein
/// `audit = "<literal>",` enthalten, in beliebiger Reihenfolge.
///
/// # Positivliste der zulässigen Feldtypen
///
/// Jedes Datenfeld muss einer dieser Formen entsprechen — sonst ist es ein
/// Compile-Fehler, `String` ausdrücklich eingeschlossen (siehe
/// `warden_actions.rs`-Moduldoku für die Begründung: ein Feld, in das ein
/// Modell freien Text schreiben kann, ist der Weg, auf dem eine
/// Modellausgabe zu einem Befehl wird):
///
/// - eine `harw-types`-ID: `ActionId`, `BaselineId`, `CgroupId`, `ChannelId`,
///   `FindingId`, `HostId`, `ItemId`, `PeerId`, `SensorId`, `SessionId`,
///   `TenantId`, `ThreadId`, `ThreadRef`, `ToolCallId`, `TurnId`, `WorkId`,
///   `WorkspaceId`;
/// - ein Ganzzahltyp: `u8`, `u16`, `u32`, `u64`, `u128`, `usize`, `i8`,
///   `i16`, `i32`, `i64`, `i128`, `isize`;
/// - `bool`;
/// - `Vec<T>` für jeden der oben genannten Typen (nicht verschachtelt,
///   nicht `Vec<String>`) — für Aktionen, die mehrere gleichartige Ziele auf
///   einmal betreffen (siehe `warden_actions.rs`-Moduldoku, Abschnitt „Warum
///   `Vec<T>`").
///
/// `Option<T>` ist bewusst nicht unterstützt; eine fehlende Angabe wird als
/// eigene Aktion ausgedrückt.
///
/// # Compile-Fehler bei
///
/// - leerer Deklaration, oder keiner Aktion nach `authorization_proof = ...;`;
/// - einem ersten Element, das nicht `authorization_proof = <Pfad>;` ist;
/// - einer Aktion **ohne** `admissible_from: [...]` — eine Aktion, die man
///   vorschlagen kann, für die aber niemand festgelegt hat, ab welcher Stufe
///   sie zulässig ist, ist eine Lücke in der Eskalationsleiter;
/// - einer Aktion **ohne** `audit = "..."` — ein Eingriff ohne Spur;
/// - einem leeren `audit`-Namen;
/// - einem Feldtyp außerhalb der Positivliste, `String` eingeschlossen;
/// - zwei Aktionen mit demselben Namen, oder zwei Feldern derselben Aktion
///   mit demselben Namen;
/// - doppelt angegebenem `admissible_from` oder `audit` innerhalb einer
///   Aktion.
///
/// # Pfadanforderungen an die aufrufende Crate
///
/// Der erzeugte Code referenziert `::serde::{Serialize, Deserialize}`,
/// `::harw_tools::{JsonSchema, JsonSchemaType, AdditionalProperties,
/// ToolSpec, FunctionToolSpec, ToolName, serde_json}`, sowie jeden in einem
/// Feldtyp oder in `authorization_proof = <Pfad>;` genannten Pfad (z. B.
/// `harw_types::CgroupId`, `harw_dod_warden_proto::AuthorizationProof`). Eine
/// Crate, die `warden_actions!` nutzt, muss deshalb von `serde` (mit
/// `derive`-Feature), `harw-tools` und jeder Crate abhängen, die einen
/// verwendeten Feld- oder Belegtyp bereitstellt — `harw-macros` selbst hängt
/// auf keine davon produktiv ab (siehe Moduldoku von `lib.rs`). Der über
/// `authorization_proof = <Pfad>;` benannte Typ muss `Debug`, `Clone`,
/// `::serde::Serialize` und `::serde::Deserialize` implementieren, da er
/// unverändert in die generierten `derive`-Listen von `WardenActionRequest`
/// eingeht.
///
/// # Examples
///
/// ```ignore
/// harw_macros::warden_actions! {
///     authorization_proof = harw_dod_warden_proto::AuthorizationProof;
///
///     /// Friert eine cgroup ein.
///     FreezeCgroup {
///         cgroup: harw_types::CgroupId,
///         admissible_from: [RuleTriggered, Escalated],
///         audit = "warden.freeze_cgroup",
///     }
///     /// Isoliert mehrere cgroups auf einmal.
///     IsolateCgroups {
///         cgroups: Vec<harw_types::CgroupId>,
///         admissible_from: [Escalated],
///         audit = "warden.isolate_cgroups",
///     }
/// }
/// ```
///
/// # Der optionale Schlüssel `tool_schema`
/// Ein zusätzlicher, optionaler Schlüssel `tool_schema = <Pfad>;` — in
/// derselben Form wie `authorization_proof = <Pfad>;` — schaltet
/// `impl ProposedAction { pub fn tool_schema() -> <Pfad> }` ein. **Fehlt der
/// Schlüssel, entsteht dieses Impl nicht, und der erzeugte Code nennt
/// `harw_tools` an keiner Stelle.**
///
/// Das ist kein Feinschliff, sondern die Bedingung dafür, dass dieses Makro
/// überhaupt einen Konsumenten hat: `harw-dod-warden-proto` liegt auf dem Weg
/// zum Warden, für den ein Gate **höchstens zwölf transitive
/// Abhängigkeiten** erlaubt. Die bedingungslose `tool_schema()`-Ausgabe zog
/// `harw-tools` samt acht zusätzlicher Crates herein — der erste Konsument hat
/// die Typen deshalb von Hand geschrieben, und das Makro stand ohne Aufrufer
/// da.
///
/// **Abwählbar durch Weglassen, nicht durch Einschalten** — ein Schlüssel, den
/// man setzen müsste, um eine Abhängigkeit *loszuwerden*, wird vergessen, und
/// das Vergessen fällt in die teure Richtung. Ein Cargo-Feature wäre der
/// falsche Weg: Features werden über den Abhängigkeitsgraphen **vereinigt**,
/// eine fremde Crate könnte `harw-tools` also über einen Umweg
/// zurückbringen, ohne dass das an der eigenen Deklaration ablesbar wäre.
///
/// Näheres im Abschnitt „`tool_schema` — abwählbar durch Weglassen, nicht
/// durch Einschalten" der Moduldokumentation von `warden_actions.rs`.
#[proc_macro]
pub fn warden_actions(input: TokenStream) -> TokenStream {
    match warden_actions::expand_warden_actions(input.into()) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
