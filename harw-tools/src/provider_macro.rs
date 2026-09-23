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

/// Erzeugt eine zustandslose Provider-Struktur mit den inhärenten Methoden
/// `tool_specs()`, `tool_executor(name)` und `tool_parallel_safe(name)`.
///
/// # Description
/// Dies ist der Kern von [`crate::tool_provider!`] und verwendet ausschließlich
/// `$crate`-Pfade, hängt also nur von `harw-tools` selbst ab. Verwende dieses
/// Makro direkt, wenn ein Crate die Tool-Menge bündeln will, ohne den
/// `ToolProvider`-Trait aus `harw-extension-api` zu implementieren.
///
/// # Syntax
/// ```rust,ignore
/// tool_provider_core! {
///     /// Doku-Kommentar (optional, beliebig viele Attribute)
///     pub struct FsToolProvider { FsReadTool, FsWriteTool }
/// }
/// ```
///
/// # Anforderungen an jeden Tool-Typ
/// Jeder aufgeführte Typ `T` muss bereitstellen:
/// - `T::NAME: &'static str`,
/// - `T::PERMISSION: Option<`[`crate::Permission`]`>`,
/// - `T::PARALLEL_SAFE: bool`,
/// - `T::spec() -> `[`crate::ToolSpec`],
/// - `impl Default for T` und `impl `[`crate::ToolExecutor`]` for T`.
///
/// Genau diese Menge erzeugt `#[harw_macros::tool]`.
///
/// # Generierte Items
/// - `$vis struct $name;` mit `Debug`, `Clone`, `Copy`, `Default`.
/// - `$name::TOOL_NAMES: &'static [&'static str]` — Namen in Deklarationsreihenfolge.
/// - `$name::TOOL_PERMISSIONS: &'static [Option<Permission>]` — parallel dazu die
///   deklarierten Berechtigungen, damit ein Crate per Test auditieren kann, dass
///   jedes registrierte Tool eine Berechtigung deklariert.
/// - `$name::new()`, `$name::tool_specs()`, `$name::tool_executor(&str)`,
///   `$name::tool_parallel_safe(&str)`.
/// - eine anonyme `const`-Assertion, die doppelte Tool-Namen zum Compile-Fehler macht.
///
/// # Doppelte Tool-Namen
/// Zwei Tools mit demselben `NAME` wären ein echter Dispatch-Fehler: `tools()`
/// würde den Namen zweimal bewerben, `executor(name)` und
/// `parallel_safe(name)` aber immer nur das zuerst deklarierte Tool treffen —
/// inklusive dessen (womöglich großzügigerer) Parallelitäts-Zusage. Die
/// Prüfung läuft deshalb als `const`-Assertion zur Compile-Zeit über
/// `TOOL_NAMES`, nicht als Test: ein Namenskonflikt darf den Build brechen,
/// nicht bloß eine Test-Suite.
///
/// # Panics
/// Der generierte Code panickt nie zur Laufzeit. Die `const`-Assertion schlägt
/// zur Compile-Zeit fehl, wenn zwei Tool-Namen übereinstimmen.
#[macro_export]
macro_rules! tool_provider_core {
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

        // Compile-Zeit-Duplikatprüfung der Tool-Namen.
        const _: () = {
            const fn __harw_str_eq(left: &str, right: &str) -> bool {
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

            const fn __harw_names_unique(names: &[&str]) -> bool {
                let mut outer = 0;
                while outer < names.len() {
                    let mut inner = outer + 1;
                    while inner < names.len() {
                        if __harw_str_eq(names[outer], names[inner]) {
                            return false;
                        }
                        inner += 1;
                    }
                    outer += 1;
                }
                true
            }

            // Die Meldung muss ein einzelnes String-Literal bleiben: `panic!` ist
            // im const-Kontext nur ohne Format-Argumente erlaubt. `concat!` und
            // `stringify!` expandieren zu genau einem Literal.
            ::core::assert!(
                __harw_names_unique(<$provider>::TOOL_NAMES),
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

/// Erzeugt eine Provider-Struktur samt `impl ToolProvider` aus einer Liste von
/// `#[harw_macros::tool]`-Tool-Typen.
///
/// # Description
/// Ruft [`crate::tool_provider_core!`] für die gesamte Logik auf und ergänzt eine
/// dünne `impl ::harw_extension_api::contributors::ToolProvider`, die
/// `ToolName` auf `&str` herunterbricht und an die inhärenten Methoden
/// delegiert.
///
/// # Syntax
/// ```rust,ignore
/// harw_tools::tool_provider! {
///     /// Stellt alle Filesystem-Tools bereit.
///     pub struct FsToolProvider {
///         FsReadTool,
///         FsWriteTool,
///         FsGlobTool,
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
/// Der erzeugte Provider ist eine Unit-Struktur und kann daher keine
/// Konfiguration tragen (z. B. `max_read_bytes`). Provider mit Konfiguration
/// bleiben handgeschrieben; das Makro adressiert den häufigen zustandslosen Fall.
///
/// # Generierte Items
/// Alles aus [`crate::tool_provider_core!`], plus
/// `impl ToolProvider for $provider` mit `tools()`, `executor(&ToolName)` und
/// `parallel_safe(&ToolName)`.
#[macro_export]
macro_rules! tool_provider {
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
}
