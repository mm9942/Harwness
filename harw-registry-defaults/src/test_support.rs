//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Ein Fehler aus `harw_registry_defaults::RegistryDefaultsError`.
    Crate(crate::error::RegistryDefaultsError),
}

pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            Self::Unexpected(message) => write!(f, "unerwartetes Ergebnis: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Crate(error) => write!(f, "{error}"),
        }
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Crate(error) => Some(error),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<crate::error::RegistryDefaultsError> for TestError {
    fn from(error: crate::error::RegistryDefaultsError) -> Self {
        Self::Crate(error)
    }
}

/// Hilfsfunktion: übersetzt `.expect("…")` in `.map_err(ctx("…"))?` und behält die
/// ursprüngliche Kontextmeldung.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}

/// Golden-Vergleich der Oberfläche eines `ToolProvider`: Namen, Spezifikations-
/// JSON, Parallelitäts-Zusagen, Executor-Auflösung und (falls der Provider sie
/// trägt) die deklarierten Berechtigungen.
///
/// Die Golden-Dateien unter `tests/golden/` wurden **vor** der Migration auf
/// `tool_provider!` vom handgeschriebenen Provider erfasst; der Test beweist,
/// dass die Migration die Oberfläche nicht verändert. Absichtliche Änderungen
/// werden mit `HARW_UPDATE_GOLDEN=1 cargo test -p <crate>` neu erfasst.
pub(crate) mod golden {
    use super::{TestError, TestResult, ctx};
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{Permission, ToolName};
    use serde_json::{Map, Value, json};
    use std::path::PathBuf;

    /// Baut die zu vergleichende Oberfläche des Providers.
    pub(crate) fn surface(
        provider: &dyn ToolProvider,
        permissions: Option<&[Option<Permission>]>,
    ) -> TestResult<Value> {
        let specs = provider.tools();
        let mut parallel = Map::new();
        let mut executors = Map::new();
        for spec in &specs {
            let name = ToolName::new(spec.name());
            parallel.insert(
                spec.name().to_owned(),
                Value::Bool(provider.parallel_safe(&name)),
            );
            executors.insert(
                spec.name().to_owned(),
                Value::Bool(provider.executor(&name).is_some()),
            );
        }
        let unknown = ToolName::new("golden.unknown");
        Ok(json!({
            "tools": serde_json::to_value(&specs).map_err(ctx("specs serialise"))?,
            "parallel_safe": parallel,
            "has_executor": executors,
            "unknown_has_executor": provider.executor(&unknown).is_some(),
            "unknown_parallel_safe": provider.parallel_safe(&unknown),
            "permissions": permissions.map(|declared| {
                declared.iter().map(|permission| format!("{permission:?}")).collect::<Vec<_>>()
            }),
        }))
    }

    /// Vergleicht `actual` mit `tests/golden/<file>`; mit `HARW_UPDATE_GOLDEN`
    /// wird die Datei stattdessen geschrieben.
    pub(crate) fn assert_golden(file: &str, actual: &Value) -> TestResult {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join(file);
        if std::env::var_os("HARW_UPDATE_GOLDEN").is_some() {
            let parent = path
                .parent()
                .ok_or(TestError::Missing("golden file has a parent directory"))?;
            std::fs::create_dir_all(parent).map_err(ctx("golden directory is creatable"))?;
            let mut text = serde_json::to_string_pretty(actual).map_err(ctx("golden serialises"))?;
            text.push('\n');
            std::fs::write(&path, text).map_err(ctx("golden file is writable"))?;
            return Ok(());
        }
        let text = std::fs::read_to_string(&path).map_err(ctx("golden file is readable"))?;
        let expected: Value = serde_json::from_str(&text).map_err(ctx("golden file parses"))?;
        if &expected != actual {
            return Err(TestError::Unexpected(format!(
                "provider surface differs from golden {file}\nexpected: {expected:#}\nactual: {actual:#}"
            )));
        }
        Ok(())
    }
}
