//! Das `tool_provider!`-Makro — deklarative Bündelung von `#[tool]`-Tools zu
//! einem `ToolProvider`.
//!
//! # Verantwortung
//! Dieses Modul besitzt die Boilerplate, die bisher in jedem Tool-Crate von
//! Hand geschrieben wurde: eine Provider-Struktur plus `tools()`,
//! `executor(name)` und `parallel_safe(name)`. Es delegiert alle inhaltlichen
//! Entscheidungen an die Tool-Typen selbst — Name, Spezifikation und
//! Parallelitäts-Zusage stammen ausschließlich aus den Consts, die
//! `#[harw_macros::tool]` erzeugt (`NAME`, `PERMISSION`, `PARALLEL_SAFE`,
//! `spec()`).
//!
//! # Schlüsseltypen
//! - [`crate::tool_provider!`] — Provider inklusive `impl ToolProvider`.
//! - [`crate::tool_provider_core!`] — nur die inhärenten Methoden, ohne
//!   Abhängigkeit auf `harw-extension-api`.
//!
//! # Warum zwei Makros?
//! `ToolProvider` lebt in `harw-extension-api`, und `harw-extension-api`
//! hängt seinerseits von `harw-tools` ab. `harw-tools` darf den Trait deshalb
//! nicht importieren (Zyklus). Die Trennung hält die gesamte Logik in
//! [`crate::tool_provider_core!`] — dort nur `$crate`-Pfade — und beschränkt
//! [`crate::tool_provider!`] auf ein dünnes Weiterreichen an den Trait. Damit ist
//! die Kernlogik innerhalb von `harw-tools` testbar, und Crates ohne
//! `harw-extension-api`-Abhängigkeit können [`crate::tool_provider_core!`] allein
//! verwenden.
//!
//! # Nebenläufigkeit
//! Die erzeugten Provider sind zustandslose Unit-Strukturen und damit
//! `Send + Sync + Copy`. Pro `executor()`-Aufruf entsteht ein frischer
//! [`std::sync::Arc`]; es gibt keinen geteilten veränderlichen Zustand.
//!
//! # Fehlertypen
//! Die Makros erzeugen keinen fehlbaren Code. Doppelte Tool-Namen sind ein
//! Compile-Fehler (siehe unten), unbekannte Namen liefern zur Laufzeit `None`
//! bzw. `false`.
//!
//! # Beispiel
//! ```rust,ignore
//! harw_tools::tool_provider! {
//!     /// Stellt alle Filesystem-Tools bereit.
//!     pub struct FsToolProvider {
//!         FsReadTool,
//!         FsWriteTool,
//!         FsGlobTool,
//!     }
//! }
//! ```

/// Interne Hilfen der Makros. Nicht Teil der stabilen API.
///
/// Die `const fn`s stehen hier statt im Makro-Rumpf, damit sie nur einmal
/// existieren und die Makros ausschließlich über `$crate`-Pfade darauf
/// zugreifen.
#[doc(hidden)]
pub mod __private {
    /// Bytegleichheit zweier Strings, `const`-tauglich.
    #[must_use]
    pub const fn str_eq(left: &str, right: &str) -> bool {
        let left = left.as_bytes();
        let right = right.as_bytes();
        if left.len() != right.len() {
            return false;
        }
        let mut index = 0;
        while index < left.len() {
            if left[index] != right[index] {
                return false;
            }
            index += 1;
        }
        true
    }

    /// `true`, wenn kein Name in `names` doppelt vorkommt.
    #[must_use]
    pub const fn names_unique(names: &[&str]) -> bool {
        let mut outer = 0;
        while outer < names.len() {
            let mut inner = outer + 1;
            while inner < names.len() {
                if str_eq(names[outer], names[inner]) {
                    return false;
                }
                inner += 1;
            }
            outer += 1;
        }
        true
    }

    /// `true`, wenn `needle` in `names` vorkommt.
    #[must_use]
    pub const fn names_contain(names: &[&str], needle: &str) -> bool {
        let mut index = 0;
        while index < names.len() {
            if str_eq(names[index], needle) {
                return true;
            }
            index += 1;
        }
        false
    }
}

/// Compile-Zeit-Duplikatprüfung der Tool-Namen (intern, von
/// [`crate::tool_provider_core!`] aufgerufen).
///
/// Die Meldung muss ein einzelnes String-Literal bleiben: `panic!` ist im
/// const-Kontext nur ohne Format-Argumente erlaubt. `concat!` und
/// `stringify!` expandieren zu genau einem Literal.
#[doc(hidden)]
#[macro_export]
macro_rules! __harw_assert_unique_tool_names {
    ($provider:ty, $names:expr) => {
        const _: () = {
            ::core::assert!(
                $crate::provider_macro::__private::names_unique($names),
                ::core::concat!(
                    "tool_provider!(",
                    ::core::stringify!($provider),
                    "): zwei Tools deklarieren denselben NAME — tools() würde einen Namen \
                     bewerben, den executor()/parallel_safe() nicht eindeutig auflösen können"
                )
            );
        };
    };
}

/// Intern: Auswertung der `parallel_safe: none | all | [..]`-Angabe der
/// Impl-Form von [`crate::tool_provider_core!`]. Fehlt die Angabe, gilt `none`.
/// Unbekannte Namen sind in jedem Fall `false` (fail closed).
#[doc(hidden)]
#[macro_export]
macro_rules! __harw_parallel_safe {
    (() $name:ident; $($known:expr),+) => {
        false
    };
    ((none) $name:ident; $($known:expr),+) => {
        false
    };
    ((all) $name:ident; $($known:expr),+) => {
        $( $name == $known )||+
    };
    (([]) $name:ident; $($known:expr),+) => {
        false
    };
    (([ $($listed:expr),+ $(,)? ]) $name:ident; $($known:expr),+) => {
        ( $( $name == $listed )||+ ) && ( $( $name == $known )||+ )
    };
}

/// Intern: jeder in `parallel_safe: [..]` genannte Name muss ein Tool des
/// Providers sein — sonst wäre die Zusage ein stiller Tippfehler.
#[doc(hidden)]
#[macro_export]
macro_rules! __harw_assert_parallel_listed {
    (() $provider:ty, $names:expr) => {};
    ((none) $provider:ty, $names:expr) => {};
    ((all) $provider:ty, $names:expr) => {};
    (([]) $provider:ty, $names:expr) => {};
    (([ $($listed:expr),+ $(,)? ]) $provider:ty, $names:expr) => {
        const _: () = {
            $(
                ::core::assert!(
                    $crate::provider_macro::__private::names_contain($names, $listed),
                    ::core::concat!(
                        "tool_provider!(",
                        ::core::stringify!($provider),
                        "): `parallel_safe` nennt ein Tool, das der Provider nicht deklariert"
                    )
                );
            )+
        };
    };
}

/// Intern: optionale `permission:`-Angabe der Impl-Form (`None`, wenn sie fehlt).
#[doc(hidden)]
#[macro_export]
macro_rules! __harw_permission_of {
    () => {
        ::core::option::Option::None
    };
    ($permission:expr) => {
        ::core::option::Option::Some($permission)
    };
}

/// Erzeugt die inhärenten Methoden eines Providers: `tool_specs()`,
/// `tool_executor(name)` und `tool_parallel_safe(name)`.
///
/// # Description
/// Dies ist der Kern von [`crate::tool_provider!`] und verwendet ausschließlich
/// `$crate`-Pfade, hängt also nur von `harw-tools` selbst ab. Verwende dieses
/// Makro direkt, wenn ein Crate die Tool-Menge bündeln will, ohne den
/// `ToolProvider`-Trait aus `harw-extension-api` zu implementieren.
///
/// Es gibt drei Formen.
///
/// # Form 1 — zustandslose Unit-Struktur
/// ```rust,ignore
/// tool_provider_core! {
///     /// Doku-Kommentar (optional, beliebig viele Attribute)
///     pub struct FsToolProvider { FsReadTool, FsWriteTool }
/// }
/// ```
/// Jeder aufgeführte Typ `T` muss bereitstellen:
/// - `T::NAME: &'static str`,
/// - `T::PERMISSION: Option<`[`crate::Permission`]`>`,
/// - `T::PARALLEL_SAFE: bool`,
/// - `T::spec() -> `[`crate::ToolSpec`],
/// - `impl Default for T` und `impl `[`crate::ToolExecutor`]` for T`.
///
/// Genau diese Menge erzeugt `#[harw_macros::tool]`. Generiert werden
/// `$name::new()`, `$name::tool_specs()`, `$name::tool_executor(&str)` und
/// `$name::tool_parallel_safe(&str)` (alle ohne `self`).
///
/// # Form 2 — Struktur mit Zustand (`state`)
/// ```rust,ignore
/// tool_provider_core! {
///     pub struct StoreToolProvider {
///         state: Arc<Store> as store;
///         StoreReadTool => StoreReadTool::new(Arc::clone(store)),
///         StoreWriteTool => StoreWriteTool::new(Arc::clone(store)),
///     }
/// }
/// ```
/// Der Provider trägt den Zustand `state` (Typ muss `Debug + Clone` sein).
/// Statt `Default` wird pro Tool der **Konstruktor-Ausdruck** hinter `=>`
/// ausgewertet; `store` ist darin eine `&State`-Bindung (Name frei wählbar).
/// Generiert wird zusätzlich `new(state)` und `state()`; `tool_executor` ist
/// hier eine Methode mit `&self`. Die Tool-Typen brauchen kein `Default`
/// (z. B. `#[tool(state = ..)]`-Tools), sonst gelten dieselben Anforderungen
/// wie in Form 1.
///
/// # Form 3 — bestehender Typ, extern gebaute Spezifikationen (`impl for`)
/// ```rust,ignore
/// tool_provider_core! {
///     impl for PlanToolProvider as provider, parallel_safe: none {  // none | all | [NAME, ..]; Default none
///         PLAN_WRITE_TOOL => {
///             spec: provider.write_spec(),           // ToolSpec, beliebiger Ausdruck
///             permission: Permission::ReadWorkspace, // optional; fehlt es: None
///             executor: PlanToolExecutor::new(provider.clone()),
///         },
///     }
/// }
/// ```
/// Für Provider, die schon als eigene Struktur mit Feldern und Buildern
/// existieren und deren Spezifikationen nicht aus einem `#[tool]`-Args-Typ
/// stammen (`schema_from`, dynamisch gebaute Schemata). Das Makro erzeugt
/// **nur** `TOOL_NAMES`, `TOOL_PERMISSIONS`, `tool_specs(&self)`,
/// `tool_executor(&self, &str)` und `tool_parallel_safe(&str)`; die
/// Struktur bleibt handgeschrieben. `provider` ist `&Self`. `executor:` ist
/// der Executor-Wert (wird in `Arc` gelegt). `parallel_safe: all` bedeutet
/// „jedes deklarierte Tool", `[A, B]` nur die genannten (Compile-Fehler, wenn
/// ein genannter Name nicht deklariert ist); unbekannte Namen sind immer
/// `false`.
///
/// # Generierte Items (alle Formen)
/// - `$name::TOOL_NAMES: &'static [&'static str]` — Namen in Deklarationsreihenfolge.
/// - `$name::TOOL_PERMISSIONS: &'static [Option<Permission>]` — parallel dazu die
///   deklarierten Berechtigungen, damit ein Crate per Test auditieren kann, dass
///   jedes registrierte Tool eine Berechtigung deklariert.
/// - eine anonyme `const`-Assertion, die doppelte Tool-Namen zum Compile-Fehler macht.
///
/// # Doppelte Tool-Namen
/// Zwei Tools mit demselben `NAME` wären ein echter Dispatch-Fehler: `tools()`
/// würde den Namen zweimal bewerben, `executor(name)` und
/// `parallel_safe(name)` aber immer nur das zuerst deklarierte Tool treffen —
/// inklusive dessen (womöglich großzügigerer) Parallelitäts-Zusage. Die
/// Prüfung läuft deshalb als `const`-Assertion zur Compile-Zeit über
/// `TOOL_NAMES`, nicht als Test: ein Namenskonflikt darf den Build brechen,
/// nicht bloß eine Test-Suite. Das gilt für alle drei Formen.
///
/// # Panics
/// Der generierte Code panickt nie zur Laufzeit. Die `const`-Assertion schlägt
/// zur Compile-Zeit fehl, wenn zwei Tool-Namen übereinstimmen.
#[macro_export]
macro_rules! tool_provider_core {
    // Form 2: Struktur mit Zustand.
    (
        $(#[$provider_meta:meta])*
        $vis:vis struct $provider:ident {
            state: $state:ty as $st:ident;
            $( $tool:ty => $ctor:expr ),+ $(,)?
        }
    ) => {
        $(#[$provider_meta])*
        #[derive(::core::fmt::Debug, ::core::clone::Clone)]
        $vis struct $provider {
            state: $state,
        }

        impl $provider {
            /// Die Namen aller Tools dieses Providers, in Deklarationsreihenfolge.
            pub const TOOL_NAMES: &'static [&'static str] = &[ $( <$tool>::NAME ),+ ];

            /// Die von den Tools deklarierten Berechtigungen, indexgleich zu
            /// [`Self::TOOL_NAMES`].
            pub const TOOL_PERMISSIONS: &'static [::core::option::Option<$crate::Permission>] =
                &[ $( <$tool>::PERMISSION ),+ ];

            /// Erzeugt den Provider über dem gemeinsamen Zustand.
            #[must_use]
            pub fn new(state: $state) -> Self {
                Self { state }
            }

            /// Der gemeinsame Zustand, aus dem die Executor gebaut werden.
            #[must_use]
            pub fn state(&self) -> &$state {
                &self.state
            }

            /// Die Spezifikationen aller Tools, in Deklarationsreihenfolge.
            #[must_use]
            pub fn tool_specs() -> ::std::vec::Vec<$crate::ToolSpec> {
                ::std::vec![ $( <$tool>::spec() ),+ ]
            }

            /// Löst einen Tool-Namen in einen Executor auf; `None` für
            /// unbekannte Namen.
            #[must_use]
            pub fn tool_executor(
                &self,
                name: &str,
            ) -> ::core::option::Option<::std::sync::Arc<dyn $crate::ToolExecutor>> {
                let $st = &self.state;
                $(
                    if name == <$tool>::NAME {
                        let executor: ::std::sync::Arc<dyn $crate::ToolExecutor> =
                            ::std::sync::Arc::new($ctor);
                        return ::core::option::Option::Some(executor);
                    }
                )+
                ::core::option::Option::None
            }

            /// Ob nebenläufige Aufrufe dieses Tools sicher sind; unbekannte
            /// Namen liefern `false` (fail closed).
            #[must_use]
            pub fn tool_parallel_safe(name: &str) -> bool {
                $(
                    if name == <$tool>::NAME {
                        return <$tool>::PARALLEL_SAFE;
                    }
                )+
                false
            }
        }

        $crate::__harw_assert_unique_tool_names!($provider, <$provider>::TOOL_NAMES);
    };

    // Form 3: bestehender Typ, extern gebaute Spezifikationen.
    (
        impl for $provider:ty as $this:ident $( , parallel_safe: $ps:tt )? {
            $(
                $name:expr => {
                    spec: $spec:expr,
                    $( permission: $perm:expr, )?
                    executor: $exec:expr $(,)?
                }
            ),+ $(,)?
        }
    ) => {
        impl $provider {
            /// Die Namen aller Tools dieses Providers, in Deklarationsreihenfolge.
            pub const TOOL_NAMES: &'static [&'static str] = &[ $( $name ),+ ];

            /// Die deklarierten Berechtigungen, indexgleich zu [`Self::TOOL_NAMES`].
            pub const TOOL_PERMISSIONS: &'static [::core::option::Option<$crate::Permission>] =
                &[ $( $crate::__harw_permission_of!($($perm)?) ),+ ];

            /// Die Spezifikationen aller Tools, in Deklarationsreihenfolge.
            #[must_use]
            #[allow(clippy::unused_self)]
            pub fn tool_specs(&self) -> ::std::vec::Vec<$crate::ToolSpec> {
                #[allow(unused_variables)]
                let $this = self;
                ::std::vec![ $( $spec ),+ ]
            }

            /// Löst einen Tool-Namen in einen Executor auf; `None` für
            /// unbekannte Namen.
            #[must_use]
            #[allow(clippy::unused_self)]
            pub fn tool_executor(
                &self,
                name: &str,
            ) -> ::core::option::Option<::std::sync::Arc<dyn $crate::ToolExecutor>> {
                #[allow(unused_variables)]
                let $this = self;
                $(
                    if name == $name {
                        let executor: ::std::sync::Arc<dyn $crate::ToolExecutor> =
                            ::std::sync::Arc::new($exec);
                        return ::core::option::Option::Some(executor);
                    }
                )+
                ::core::option::Option::None
            }

            /// Ob nebenläufige Aufrufe dieses Tools sicher sind; unbekannte
            /// Namen liefern `false` (fail closed).
            #[must_use]
            #[allow(unused_variables)]
            pub fn tool_parallel_safe(name: &str) -> bool {
                $crate::__harw_parallel_safe!(($($ps)?) name; $( $name ),+)
            }
        }

        $crate::__harw_assert_unique_tool_names!($provider, <$provider>::TOOL_NAMES);
        $crate::__harw_assert_parallel_listed!(
            ($($ps)?) $provider, <$provider>::TOOL_NAMES
        );
    };

    // Form 1: zustandslose Unit-Struktur.
    (
        $(#[$provider_meta:meta])*
        $vis:vis struct $provider:ident { $( $tool:ty ),+ $(,)? }
    ) => {
        $(#[$provider_meta])*
        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::marker::Copy,
            ::core::default::Default,
        )]
        $vis struct $provider;

        impl $provider {
            /// Die Namen aller Tools dieses Providers, in Deklarationsreihenfolge.
            pub const TOOL_NAMES: &'static [&'static str] = &[ $( <$tool>::NAME ),+ ];

            /// Die von den Tools deklarierten Berechtigungen, indexgleich zu
            /// [`Self::TOOL_NAMES`]. `None` bedeutet, dass das Tool keinen
            /// Makro-Prolog zur Berechtigungsprüfung erzeugen lässt.
            pub const TOOL_PERMISSIONS: &'static [::core::option::Option<$crate::Permission>] =
                &[ $( <$tool>::PERMISSION ),+ ];

            /// Erzeugt den Provider. Zustandslos; alle Entscheidungen fallen pro Aufruf.
            #[must_use]
            pub fn new() -> Self {
                Self
            }

            /// Die Spezifikationen aller Tools, in Deklarationsreihenfolge.
            #[must_use]
            pub fn tool_specs() -> ::std::vec::Vec<$crate::ToolSpec> {
                ::std::vec![ $( <$tool>::spec() ),+ ]
            }

            /// Löst einen Tool-Namen in einen Executor auf.
            ///
            /// Liefert `None` für unbekannte Namen — der Aufrufer muss den Fall
            /// behandeln, statt auf ein Default-Tool zu fallen.
            #[must_use]
            pub fn tool_executor(
                name: &str,
            ) -> ::core::option::Option<::std::sync::Arc<dyn $crate::ToolExecutor>> {
                $(
                    if name == <$tool>::NAME {
                        return ::core::option::Option::Some(::std::sync::Arc::new(
                            <$tool as ::core::default::Default>::default(),
                        ));
                    }
                )+
                ::core::option::Option::None
            }

            /// Ob nebenläufige Aufrufe dieses Tools sicher sind.
            ///
            /// Unbekannte Namen liefern `false` (fail closed).
            #[must_use]
            pub fn tool_parallel_safe(name: &str) -> bool {
                $(
                    if name == <$tool>::NAME {
                        return <$tool>::PARALLEL_SAFE;
                    }
                )+
                false
            }
        }

        $crate::__harw_assert_unique_tool_names!($provider, <$provider>::TOOL_NAMES);
    };
}

/// Erzeugt eine Provider-Struktur (oder ergänzt einen bestehenden Typ) samt
/// `impl ToolProvider`.
///
/// # Description
/// Ruft [`crate::tool_provider_core!`] für die gesamte Logik auf und ergänzt eine
/// dünne `impl ::harw_extension_api::contributors::ToolProvider`, die
/// `ToolName` auf `&str` herunterbricht und an die inhärenten Methoden
/// delegiert. Alle drei Formen von [`crate::tool_provider_core!`] werden
/// unterstützt; die Syntax ist dieselbe.
///
/// # Syntax
/// ```rust,ignore
/// // Form 1: zustandslos
/// harw_tools::tool_provider! {
///     /// Stellt alle Filesystem-Tools bereit.
///     pub struct FsToolProvider { FsReadTool, FsWriteTool, FsGlobTool }
/// }
///
/// // Form 2: Zustand + Konstruktor-Ausdruck je Tool
/// harw_tools::tool_provider! {
///     pub struct StoreToolProvider {
///         state: Arc<Store> as store;
///         StoreReadTool => StoreReadTool::new(Arc::clone(store)),
///     }
/// }
///
/// // Form 3: bestehender Typ, extern gebaute Spezifikationen
/// harw_tools::tool_provider! {
///     impl for PlanToolProvider as provider, parallel_safe: none {
///         PLAN_WRITE_TOOL => {
///             spec: provider.write_spec(),
///             permission: Permission::ReadWorkspace,
///             executor: PlanToolExecutor::new(provider.clone()),
///         },
///     }
/// }
/// ```
///
/// # Pfad-Anforderungen an das aufrufende Crate
/// Die erzeugten Tokens verweisen auf `::harw_extension_api::contributors::ToolProvider`
/// sowie auf `$crate::{Permission, ToolExecutor, ToolName, ToolSpec}`. Ein Crate,
/// das dieses Makro verwendet, muss deshalb sowohl `harw-extension-api` als auch
/// `harw-tools` als Dependency führen. Wer `harw-extension-api` nicht führen will
/// oder kann, verwendet [`crate::tool_provider_core!`] direkt.
///
/// # Grenzen
/// Form 1 ist eine Unit-Struktur ohne Konfiguration. Konfiguration trägt
/// Form 2 (ein Zustandswert) oder Form 3 (eigene Struktur). Provider, deren
/// Tool-Menge erst zur Laufzeit feststeht (z. B. aus Deskriptoren), passen in
/// keine Form und bleiben handgeschrieben.
///
/// # Generierte Items
/// Alles aus [`crate::tool_provider_core!`], plus
/// `impl ToolProvider for $provider` mit `tools()`, `executor(&ToolName)` und
/// `parallel_safe(&ToolName)`.
#[macro_export]
macro_rules! tool_provider {
    // Form 2: Struktur mit Zustand.
    (
        $(#[$provider_meta:meta])*
        $vis:vis struct $provider:ident {
            state: $state:ty as $st:ident;
            $( $tool:ty => $ctor:expr ),+ $(,)?
        }
    ) => {
        $crate::tool_provider_core! {
            $(#[$provider_meta])*
            $vis struct $provider {
                state: $state as $st;
                $( $tool => $ctor ),+
            }
        }

        impl ::harw_extension_api::contributors::ToolProvider for $provider {
            fn tools(&self) -> ::std::vec::Vec<$crate::ToolSpec> {
                <$provider>::tool_specs()
            }

            fn executor(
                &self,
                name: &$crate::ToolName,
            ) -> ::core::option::Option<::std::sync::Arc<dyn $crate::ToolExecutor>> {
                self.tool_executor(name.as_str())
            }

            fn parallel_safe(&self, name: &$crate::ToolName) -> bool {
                <$provider>::tool_parallel_safe(name.as_str())
            }
        }
    };

    // Form 3: bestehender Typ.
    (
        impl for $provider:ty as $this:ident $( , parallel_safe: $ps:tt )? {
            $(
                $name:expr => {
                    spec: $spec:expr,
                    $( permission: $perm:expr, )?
                    executor: $exec:expr $(,)?
                }
            ),+ $(,)?
        }
    ) => {
        $crate::tool_provider_core! {
            impl for $provider as $this $( , parallel_safe: $ps )? {
                $(
                    $name => {
                        spec: $spec,
                        $( permission: $perm, )?
                        executor: $exec
                    }
                ),+
            }
        }

        impl ::harw_extension_api::contributors::ToolProvider for $provider {
            fn tools(&self) -> ::std::vec::Vec<$crate::ToolSpec> {
                self.tool_specs()
            }

            fn executor(
                &self,
                name: &$crate::ToolName,
            ) -> ::core::option::Option<::std::sync::Arc<dyn $crate::ToolExecutor>> {
                self.tool_executor(name.as_str())
            }

            fn parallel_safe(&self, name: &$crate::ToolName) -> bool {
                <$provider>::tool_parallel_safe(name.as_str())
            }
        }
    };

    // Form 1: zustandslose Unit-Struktur.
    (
        $(#[$provider_meta:meta])*
        $vis:vis struct $provider:ident { $( $tool:ty ),+ $(,)? }
    ) => {
        $crate::tool_provider_core! {
            $(#[$provider_meta])*
            $vis struct $provider { $( $tool ),+ }
        }

        impl ::harw_extension_api::contributors::ToolProvider for $provider {
            fn tools(&self) -> ::std::vec::Vec<$crate::ToolSpec> {
                <$provider>::tool_specs()
            }

            fn executor(
                &self,
                name: &$crate::ToolName,
            ) -> ::core::option::Option<::std::sync::Arc<dyn $crate::ToolExecutor>> {
                <$provider>::tool_executor(name.as_str())
            }

            fn parallel_safe(&self, name: &$crate::ToolName) -> bool {
                <$provider>::tool_parallel_safe(name.as_str())
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::schema::{JsonSchema, JsonSchemaType};
    use crate::spec::FunctionToolSpec;
    use crate::{
        Permission, ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName,
        ToolOutput, ToolSpec,
    };

    /// Baut die Minimal-Spezifikation, die ein `#[tool]`-erzeugtes `spec()`
    /// liefern würde. Die Dummies sind handgeschrieben, weil `#[harw_macros::tool]`
    /// absolute `::harw_tools::`-Pfade erzeugt, die innerhalb von `harw-tools`
    /// selbst nicht auflösbar sind.
    fn dummy_spec(name: &str, description: &str) -> ToolSpec {
        ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(name),
            description: description.to_owned(),
            parameters: JsonSchema {
                schema_type: Some(JsonSchemaType::Object),
                ..Default::default()
            },
            strict: false,
        })
    }

    /// Lesendes Dummy-Tool: parallelsicher, mit deklarierter Berechtigung.
    #[derive(Debug, Clone, Copy, Default)]
    struct AlphaTool;

    impl AlphaTool {
        const NAME: &'static str = "test.alpha";
        const PARALLEL_SAFE: bool = true;
        const PERMISSION: Option<Permission> = Some(Permission::ReadWorkspace);

        fn spec() -> ToolSpec {
            dummy_spec(Self::NAME, "reads something")
        }
    }

    impl ToolExecutor for AlphaTool {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move { Ok(ToolOutput::text("alpha")) })
        }
    }

    /// Schreibendes Dummy-Tool: nicht parallelsicher.
    #[derive(Debug, Clone, Copy, Default)]
    struct BetaTool;

    impl BetaTool {
        const NAME: &'static str = "test.beta";
        const PARALLEL_SAFE: bool = false;
        const PERMISSION: Option<Permission> = Some(Permission::WriteWorkspace);

        fn spec() -> ToolSpec {
            dummy_spec(Self::NAME, "writes something")
        }
    }

    impl ToolExecutor for BetaTool {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move { Ok(ToolOutput::text("beta")) })
        }
    }

    crate::tool_provider_core! {
        /// Test-Provider über zwei Dummy-Tools.
        struct TestToolProvider { AlphaTool, BetaTool }
    }

    /// Nachgestellter `#[tool]`-Fall ohne deklarierte Berechtigung — für den
    /// Audit-Test über [`TestToolProvider::TOOL_PERMISSIONS`].
    #[derive(Debug, Clone, Copy, Default)]
    struct UnguardedTool;

    impl UnguardedTool {
        const NAME: &'static str = "test.unguarded";
        const PARALLEL_SAFE: bool = false;
        const PERMISSION: Option<Permission> = None;

        fn spec() -> ToolSpec {
            dummy_spec(Self::NAME, "declares no permission")
        }
    }

    impl ToolExecutor for UnguardedTool {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move { Ok(ToolOutput::text("unguarded")) })
        }
    }

    crate::tool_provider_core! {
        /// Zweiter Provider, der ein Tool ohne Berechtigung enthält.
        struct MixedToolProvider { AlphaTool, UnguardedTool }
    }

    /// Die Namensliste folgt der Deklarationsreihenfolge.
    #[test]
    fn test_tool_provider_core_tool_names_follow_declaration_order() {
        assert_eq!(
            TestToolProvider::TOOL_NAMES,
            &["test.alpha", "test.beta"],
            "TOOL_NAMES must mirror the declaration order"
        );
    }

    /// `tool_specs()` liefert je eine Spezifikation pro Tool, mit passendem Namen.
    #[test]
    fn test_tool_provider_core_lists_one_spec_per_tool() {
        let specs = TestToolProvider::tool_specs();
        let names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();

        assert_eq!(names, vec!["test.alpha", "test.beta"]);
    }

    /// Jede Spezifikation trägt denselben Namen, unter dem `tool_executor`
    /// auflösbar ist — sonst bewirbt der Provider ein nicht erreichbares Tool.
    #[test]
    fn test_tool_provider_core_every_advertised_spec_resolves_to_an_executor() {
        for spec in TestToolProvider::tool_specs() {
            assert!(
                TestToolProvider::tool_executor(spec.name()).is_some(),
                "advertised tool {:?} must resolve to an executor",
                spec.name()
            );
        }
    }

    /// Bekannte Namen liefern einen Executor, unbekannte `None`.
    #[test]
    fn test_tool_provider_core_executor_returns_none_for_unknown_name() {
        assert!(TestToolProvider::tool_executor("test.alpha").is_some());
        assert!(TestToolProvider::tool_executor("test.beta").is_some());
        assert!(
            TestToolProvider::tool_executor("test.missing").is_none(),
            "unknown names must not fall back to any tool"
        );
    }

    /// `parallel_safe` spiegelt die Zusage des jeweiligen Tools; unbekannte
    /// Namen sind fail closed.
    #[test]
    fn test_tool_provider_core_parallel_safe_is_per_tool_and_fail_closed() {
        assert!(TestToolProvider::tool_parallel_safe("test.alpha"));
        assert!(!TestToolProvider::tool_parallel_safe("test.beta"));
        assert!(
            !TestToolProvider::tool_parallel_safe("test.missing"),
            "unknown names must default to not parallel safe"
        );
    }

    /// Die Berechtigungen stehen indexgleich zu den Namen zur Verfügung.
    #[test]
    fn test_tool_provider_core_exposes_declared_permissions() {
        assert_eq!(
            TestToolProvider::TOOL_PERMISSIONS.len(),
            TestToolProvider::TOOL_NAMES.len(),
            "permissions and names must be index-aligned"
        );
        assert_eq!(
            TestToolProvider::TOOL_PERMISSIONS,
            &[
                Some(Permission::ReadWorkspace),
                Some(Permission::WriteWorkspace),
            ]
        );
    }

    /// Beispiel für den Audit-Test, den ein Tool-Crate über seinen Provider
    /// schreiben kann: alle Tools müssen eine Berechtigung deklarieren.
    #[test]
    fn test_tool_provider_core_permission_audit_detects_unguarded_tool() {
        let unguarded: Vec<&str> = MixedToolProvider::TOOL_NAMES
            .iter()
            .zip(MixedToolProvider::TOOL_PERMISSIONS)
            .filter(|(_, permission)| permission.is_none())
            .map(|(name, _)| *name)
            .collect();

        assert_eq!(
            unguarded,
            vec!["test.unguarded"],
            "TOOL_PERMISSIONS must make an unguarded tool visible to an audit test"
        );
    }

    /// Ein Provider mit einem berechtigungsfreien Tool bleibt voll funktionsfähig.
    ///
    /// Die Audit-Tabelle allein sagt nur, dass ein solches Tool **sichtbar** ist —
    /// nicht, dass der Provider es auch bedient. Dieser Test fasst deshalb den
    /// gesamten von `tool_provider_core!` erzeugten Satz an
    /// `MixedToolProvider` an: `new`, `tool_specs`, `tool_executor` und
    /// `tool_parallel_safe`. Ohne ihn blieb genau diese Hälfte der generierten
    /// API auf diesem Provider ungeprüft — sichtbar als `dead_code`-Warnung.
    #[test]
    fn test_tool_provider_core_serves_a_provider_containing_an_unguarded_tool() {
        let provider = MixedToolProvider::new();
        assert_eq!(
            format!("{provider:?}"),
            format!("{MixedToolProvider:?}"),
            "new() muss denselben zustandslosen Provider liefern"
        );

        let specs = MixedToolProvider::tool_specs();
        let names: Vec<&str> = specs.iter().map(|spec| spec.name()).collect();
        assert_eq!(
            names,
            vec!["test.alpha", "test.unguarded"],
            "tool_specs() muss beide Tools in Deklarationsreihenfolge liefern"
        );

        // Ein Tool ohne deklarierte Berechtigung ist trotzdem auflösbar: die
        // fehlende Berechtigung ist eine Audit-Aussage, keine Abschaltung.
        assert!(
            MixedToolProvider::tool_executor("test.unguarded").is_some(),
            "ein berechtigungsfreies Tool muss dennoch einen Executor haben"
        );
        assert!(
            MixedToolProvider::tool_executor("test.fehlt").is_none(),
            "unbekannte Namen dürfen nicht auf ein Default-Tool fallen"
        );

        // `UnguardedTool::PARALLEL_SAFE` ist `false` — die generierte Abfrage
        // muss den je Tool deklarierten Wert durchreichen, nicht raten.
        assert!(MixedToolProvider::tool_parallel_safe("test.alpha"));
        assert!(!MixedToolProvider::tool_parallel_safe("test.unguarded"));
        assert!(
            !MixedToolProvider::tool_parallel_safe("test.fehlt"),
            "unbekannte Namen bleiben fail-closed"
        );
    }

    /// Der erzeugte Provider ist zustandslos und über `Default` konstruierbar.
    #[test]
    fn test_tool_provider_core_provider_is_default_constructible() {
        let from_new = TestToolProvider::new();
        let from_default = TestToolProvider;

        assert_eq!(
            format!("{from_new:?}"),
            format!("{from_default:?}"),
            "new() and default() must produce the same stateless provider"
        );
    }

    // ── Form 2: Zustand ──────────────────────────────────────────────────────

    /// Zustand, den der Provider an seine Executor weiterreicht.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Counter {
        label: &'static str,
    }

    /// Tool mit Zustand: kein `Default`, Konstruktor nimmt den Zustand.
    #[derive(Debug, Clone)]
    struct StatefulTool {
        label: &'static str,
    }

    impl StatefulTool {
        const NAME: &'static str = "test.stateful";
        const PARALLEL_SAFE: bool = true;
        const PERMISSION: Option<Permission> = Some(Permission::ReadWorkspace);

        fn new(state: &Counter) -> Self {
            Self { label: state.label }
        }

        fn spec() -> ToolSpec {
            dummy_spec(Self::NAME, "uses state")
        }
    }

    impl ToolExecutor for StatefulTool {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            let label = self.label;
            Box::pin(async move { Ok(ToolOutput::text(label)) })
        }
    }

    crate::tool_provider_core! {
        /// Test-Provider mit Zustand.
        struct StatefulProvider {
            state: Counter as counter;
            StatefulTool => StatefulTool::new(counter),
            BetaTool => BetaTool,
        }
    }

    /// Form 2 trägt den Zustand und löst beide Tools auf.
    #[test]
    fn test_tool_provider_core_state_form_carries_state_and_resolves_tools() {
        let provider = StatefulProvider::new(Counter { label: "gamma" });

        assert_eq!(provider.state(), &Counter { label: "gamma" });
        assert_eq!(
            StatefulProvider::TOOL_NAMES,
            &["test.stateful", "test.beta"]
        );
        assert!(provider.tool_executor("test.stateful").is_some());
        assert!(provider.tool_executor("test.beta").is_some());
        assert!(provider.tool_executor("test.missing").is_none());
        assert!(StatefulProvider::tool_parallel_safe("test.stateful"));
        assert!(!StatefulProvider::tool_parallel_safe("test.beta"));
        assert!(!StatefulProvider::tool_parallel_safe("test.missing"));
        assert_eq!(
            StatefulProvider::TOOL_PERMISSIONS,
            &[
                Some(Permission::ReadWorkspace),
                Some(Permission::WriteWorkspace)
            ]
        );
        let specs = StatefulProvider::tool_specs();
        let names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
        assert_eq!(names, vec!["test.stateful", "test.beta"]);
    }

    // ── Form 3: bestehender Typ, externe Spezifikationen ─────────────────────

    /// Handgeschriebener Provider mit Konfiguration.
    #[derive(Debug, Clone)]
    struct ConfiguredProvider {
        description: &'static str,
    }

    #[derive(Debug)]
    struct ParallelAll;

    #[derive(Debug)]
    struct ParallelDefault;

    #[derive(Debug)]
    struct ParallelNone;

    const EXT_READ: &str = "ext.read";
    const EXT_WRITE: &str = "ext.write";
    const EXT_PLAIN: &str = "ext.plain";

    crate::tool_provider_core! {
        impl for ConfiguredProvider as provider, parallel_safe: [EXT_READ] {
            EXT_READ => {
                spec: dummy_spec(EXT_READ, provider.description),
                permission: Permission::ReadWorkspace,
                executor: AlphaTool,
            },
            EXT_WRITE => {
                spec: dummy_spec(EXT_WRITE, "writes"),
                permission: Permission::WriteWorkspace,
                executor: BetaTool,
            },
            EXT_PLAIN => {
                spec: dummy_spec(EXT_PLAIN, "no permission"),
                executor: UnguardedTool,
            }
        }
    }

    crate::tool_provider_core! {
        impl for ParallelAll as _provider, parallel_safe: all {
            EXT_READ => { spec: dummy_spec(EXT_READ, "r"), executor: AlphaTool },
            EXT_WRITE => { spec: dummy_spec(EXT_WRITE, "w"), executor: BetaTool }
        }
    }

    crate::tool_provider_core! {
        impl for ParallelDefault as _provider {
            EXT_READ => { spec: dummy_spec(EXT_READ, "r"), executor: AlphaTool }
        }
    }

    crate::tool_provider_core! {
        impl for ParallelNone as _provider, parallel_safe: none {
            EXT_READ => { spec: dummy_spec(EXT_READ, "r"), executor: AlphaTool }
        }
    }

    /// Form 3 liest die Spezifikation aus dem Provider-Zustand und behält die
    /// Deklarationsreihenfolge.
    #[test]
    fn test_tool_provider_core_impl_form_builds_specs_from_provider_state() {
        let provider = ConfiguredProvider {
            description: "configured",
        };
        let specs = provider.tool_specs();
        let names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();

        assert_eq!(names, vec![EXT_READ, EXT_WRITE, EXT_PLAIN]);
        assert_eq!(
            ConfiguredProvider::TOOL_NAMES,
            &[EXT_READ, EXT_WRITE, EXT_PLAIN]
        );
        let descriptions: Vec<&str> = specs
            .iter()
            .map(|ToolSpec::Function(function)| function.description.as_str())
            .collect();
        assert_eq!(descriptions, vec!["configured", "writes", "no permission"]);
    }

    /// `permission:` ist optional; fehlt es, steht `None` an der Stelle.
    #[test]
    fn test_tool_provider_core_impl_form_permissions_default_to_none() {
        assert_eq!(
            ConfiguredProvider::TOOL_PERMISSIONS,
            &[
                Some(Permission::ReadWorkspace),
                Some(Permission::WriteWorkspace),
                None
            ]
        );
    }

    /// Executor lösen bekannte Namen auf, unbekannte nicht.
    #[test]
    fn test_tool_provider_core_impl_form_resolves_executors() {
        let provider = ConfiguredProvider { description: "d" };

        assert!(provider.tool_executor(EXT_READ).is_some());
        assert!(provider.tool_executor(EXT_PLAIN).is_some());
        assert!(provider.tool_executor("ext.missing").is_none());
    }

    /// `parallel_safe: [..]` gilt nur für genannte Namen; unbekannte sind `false`.
    #[test]
    fn test_tool_provider_core_impl_form_parallel_list_is_exact_and_fail_closed() {
        assert!(ConfiguredProvider::tool_parallel_safe(EXT_READ));
        assert!(!ConfiguredProvider::tool_parallel_safe(EXT_WRITE));
        assert!(!ConfiguredProvider::tool_parallel_safe(EXT_PLAIN));
        assert!(!ConfiguredProvider::tool_parallel_safe("ext.missing"));
    }

    /// `all`, `none` und die fehlende Angabe.
    #[test]
    fn test_tool_provider_core_impl_form_parallel_all_none_and_default() {
        assert!(ParallelAll::tool_parallel_safe(EXT_READ));
        assert!(ParallelAll::tool_parallel_safe(EXT_WRITE));
        assert!(
            !ParallelAll::tool_parallel_safe("ext.missing"),
            "`all` must not make unknown names parallel safe"
        );
        assert!(!ParallelNone::tool_parallel_safe(EXT_READ));
        assert!(
            !ParallelDefault::tool_parallel_safe(EXT_READ),
            "omitted parallel_safe must default to none"
        );
    }

    /// Die Hilfs-Provider bedienen dieselbe generierte API wie der Haupt-Provider.
    #[test]
    fn test_tool_provider_core_impl_form_small_providers_serve_generated_api() {
        assert_eq!(ParallelAll::TOOL_PERMISSIONS, &[None, None]);
        assert_eq!(ParallelDefault::TOOL_PERMISSIONS, &[None]);
        assert_eq!(ParallelNone::TOOL_PERMISSIONS, &[None]);
        assert_eq!(ParallelAll.tool_specs().len(), 2);
        assert_eq!(ParallelDefault.tool_specs().len(), 1);
        assert_eq!(ParallelNone.tool_specs().len(), 1);
        assert!(ParallelAll.tool_executor(EXT_WRITE).is_some());
        assert!(ParallelDefault.tool_executor(EXT_READ).is_some());
        assert!(ParallelNone.tool_executor(EXT_WRITE).is_none());
    }
}
