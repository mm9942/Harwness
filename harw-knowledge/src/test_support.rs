//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

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
    /// Ein Fehler aus `harw-knowledge` selbst (ersetzt `.unwrap()`/`.unwrap_err()`
    /// auf [`crate::error::KnowledgeResult`]).
    Knowledge(crate::error::KnowledgeError),
    /// Ein I/O-Fehler (Temp-Verzeichnisse, Dateizugriffe in Store-Tests).
    Io(std::io::Error),
    /// Ein JSON-Fehler (Serialisierung/Deserialisierung in Tests).
    Json(serde_json::Error),
    /// Ein YAML-Frontmatter-Fehler (`serde_norway`).
    Frontmatter(serde_norway::Error),
    /// Ein TOML-Serialisierungsfehler (`board.toml` schreiben).
    TomlEncode(toml::ser::Error),
    /// Ein TOML-Deserialisierungsfehler (`board.toml` lesen).
    TomlDecode(toml::de::Error),
    /// Ein Fehler aus `harw-job-runtime`.
    Job(harw_job_runtime::JobError),
    /// Ein Zeitarithmetik-Fehler (`jiff`).
    Time(jiff::Error),
}

pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "fehlender Wert: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Knowledge(source) => write!(f, "Knowledge-Fehler: {source}"),
            TestError::Io(source) => write!(f, "I/O-Fehler: {source}"),
            TestError::Json(source) => write!(f, "JSON-Fehler: {source}"),
            TestError::Frontmatter(source) => write!(f, "Frontmatter-Fehler: {source}"),
            TestError::TomlEncode(source) => write!(f, "TOML-Encode-Fehler: {source}"),
            TestError::TomlDecode(source) => write!(f, "TOML-Decode-Fehler: {source}"),
            TestError::Job(source) => write!(f, "Job-Fehler: {source}"),
            TestError::Time(source) => write!(f, "Zeit-Fehler: {source}"),
        }
    }
}

impl std::fmt::Debug for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TestError::Knowledge(source) => Some(source),
            TestError::Io(source) => Some(source),
            TestError::Json(source) => Some(source),
            TestError::Frontmatter(source) => Some(source),
            TestError::TomlEncode(source) => Some(source),
            TestError::TomlDecode(source) => Some(source),
            TestError::Job(source) => Some(source),
            TestError::Time(source) => Some(source),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<crate::error::KnowledgeError> for TestError {
    fn from(source: crate::error::KnowledgeError) -> Self {
        TestError::Knowledge(source)
    }
}

impl From<std::io::Error> for TestError {
    fn from(source: std::io::Error) -> Self {
        TestError::Io(source)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(source: serde_json::Error) -> Self {
        TestError::Json(source)
    }
}

impl From<serde_norway::Error> for TestError {
    fn from(source: serde_norway::Error) -> Self {
        TestError::Frontmatter(source)
    }
}

impl From<toml::ser::Error> for TestError {
    fn from(source: toml::ser::Error) -> Self {
        TestError::TomlEncode(source)
    }
}

impl From<toml::de::Error> for TestError {
    fn from(source: toml::de::Error) -> Self {
        TestError::TomlDecode(source)
    }
}

impl From<harw_job_runtime::JobError> for TestError {
    fn from(source: harw_job_runtime::JobError) -> Self {
        TestError::Job(source)
    }
}

impl From<jiff::Error> for TestError {
    fn from(source: jiff::Error) -> Self {
        TestError::Time(source)
    }
}

/// Baut aus einem statischen Kontext eine `map_err`-Closure (ersetzt `.expect("…")`).
///
/// # Arguments
/// - `context` (`&'static str`): was versucht wurde.
///
/// # Returns
/// Eine Closure, die einen fremden Fehler in [`TestError::Context`] übersetzt.
#[allow(dead_code)] // nicht jede Testdatei, die dieses Modul einbindet, nutzt `ctx`.
pub(crate) fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}
