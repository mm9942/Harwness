//! Gemeinsames, ausschließlich für Tests gedachtes Gerüst.
//!
//! Ersetzt die in jedem Crate kopierte `test_support.rs` (`TestError`,
//! `TestResult`, `ctx`) sowie handgeschriebene Temp-Pfad-Helfer. Panic-frei:
//! weder die Makro-Ausgabe noch die Helfer nutzen `unwrap`/`expect`/`panic!`.
//!
//! Nur als `[dev-dependencies]` verwenden.
//!
//! # Beispiel
//! ```rust,ignore
//! // src/test_support.rs
//! harw_test_support::define_test_error!(pub(crate));
//! // mit zusätzlichen `From`-Varianten:
//! // harw_test_support::define_test_error!(pub(crate), Io(std::io::Error) => "I/O-Fehler");
//! ```

mod tmp;

pub use tmp::{unique_tmp, unique_tmp_created};

/// Erzeugt `TestError`, `TestResult`, `ctx()` und `missing()` im aufrufenden Modul.
///
/// Syntax: `define_test_error!(<sichtbarkeit>)` oder
/// `define_test_error!(<sichtbarkeit>, Variante(Typ) => "Präfix", ...)`.
/// Jede Zusatzvariante erhält `From<Typ>`, eine `Display`-Ausgabe `"Präfix: {fehler}"`
/// und wird von `Error::source` zurückgegeben. `Debug` delegiert an `Display`.
#[macro_export]
macro_rules! define_test_error {
    ($vis:vis $(, $($variant:ident($ty:ty) => $label:literal),+)? $(,)?) => {
        /// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben.
        #[allow(dead_code)]
        $vis enum TestError {
            /// Ein erwarteter Wert fehlte (`Option` war `None`).
            Missing(&'static str),
            /// Ein Ergebnis hatte eine unerwartete Form.
            Unexpected(::std::string::String),
            /// Ein Fremdfehler mit Kontext (ersetzt `expect("…")`).
            Context {
                /// Was gerade versucht wurde.
                context: &'static str,
                /// Gerenderter Quellfehler.
                source: ::std::string::String,
            },
            $($(
                /// Fehler aus einer Test-Fixture oder einem Fremd-Crate.
                $variant($ty),
            )*)?
        }

        /// Ergebnis einer Testfunktion bzw. eines Test-Helfers.
        #[allow(dead_code)]
        $vis type TestResult<T = ()> = ::core::result::Result<T, TestError>;

        impl ::core::fmt::Display for TestError {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self {
                    Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
                    Self::Unexpected(message) => write!(f, "unerwartetes Ergebnis: {message}"),
                    Self::Context { context, source } => write!(f, "{context}: {source}"),
                    $($(
                        Self::$variant(error) => write!(f, "{}: {}", $label, error),
                    )*)?
                }
            }
        }

        // Debug delegiert an Display (Bible R081), damit fehlgeschlagene Tests lesbar bleiben.
        impl ::core::fmt::Debug for TestError {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                ::core::fmt::Display::fmt(self, f)
            }
        }

        impl ::std::error::Error for TestError {
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                match self {
                    Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => {
                        ::core::option::Option::None
                    }
                    $($(
                        Self::$variant(error) => ::core::option::Option::Some(error),
                    )*)?
                }
            }
        }

        $($(
            impl ::core::convert::From<$ty> for TestError {
                fn from(error: $ty) -> Self {
                    Self::$variant(error)
                }
            }
        )*)?

        /// Liefert einen `map_err`-Adapter, der einen Fremdfehler mit Kontext versieht.
        #[allow(dead_code)]
        $vis fn ctx<E: ::core::fmt::Display>(
            context: &'static str,
        ) -> impl ::core::ops::FnOnce(E) -> TestError {
            move |error| TestError::Context {
                context,
                source: error.to_string(),
            }
        }

        /// Liefert einen `ok_or_else`-Adapter für fehlende `Option`-Werte.
        #[allow(dead_code)]
        $vis fn missing(what: &'static str) -> impl ::core::ops::FnOnce() -> TestError {
            move || TestError::Missing(what)
        }
    };
}

#[cfg(test)]
mod tests {
    mod plain {
        define_test_error!(pub(crate));

        pub(crate) fn run() -> TestResult<String> {
            let none: Option<u8> = None;
            let err = none.ok_or_else(missing("wert")).err();
            let text = "x".parse::<u8>().map_err(ctx("parsen")).err();
            match (err, text) {
                (Some(a), Some(b)) => Ok(format!("{a} | {b:?}")),
                _ => Err(TestError::Unexpected("kein Fehler".into())),
            }
        }
    }

    mod extra {
        define_test_error!(pub(crate), Io(std::io::Error) => "I/O-Fehler");

        pub(crate) fn run() -> TestResult<bool> {
            let e: TestError = std::io::Error::other("boom").into();
            let rendered = e.to_string();
            let has_source = std::error::Error::source(&e).is_some();
            Ok(rendered == "I/O-Fehler: boom" && has_source)
        }
    }

    #[test]
    fn plain_renders_missing_and_context() -> Result<(), String> {
        let out = plain::run().map_err(|e| e.to_string())?;
        if out.starts_with("erwarteter Wert fehlt: wert | parsen: ") {
            Ok(())
        } else {
            Err(out)
        }
    }

    #[test]
    fn extra_variant_converts_and_has_source() -> Result<(), String> {
        match extra::run() {
            Ok(true) => Ok(()),
            Ok(false) => Err("falsches Rendering".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}
