//! Source-aware diagnostics with stable codes (`agent-definition-dsl.md` §20).
//!
//! Every problem the IR v2 lowering ([`crate::lower_v2`]) or the resolver
//! reports carries a stable code `HARW-<AREA>-NNN`, a [`Severity`], a message,
//! an optional help text, an optional source location ([`SourceSpan`]:
//! file, line, column) and an optional field path inside the definition.
//!
//! # The catalog is authoritative
//! [`CATALOG`] lists every code this crate emits. A code is never reused for
//! a different meaning; a retired code stays in the catalog and is marked as
//! retired in its title. Areas follow `agent-definition-dsl.md` §20.1:
//! `PARSE`, `SCHEMA`, `RESOLVE`, `PATCH`, `AUTH`, `ROLE`, `TOOL`, `CTX`,
//! `RETURN`, `MODEL`, `SKILL`, `BINARY`, `ORG`, `BUILD`. `ORG` and `BUILD` are
//! reserved here: organizations and compiler backends report their own codes
//! from their own crates, this crate defines none of them yet.
//!
//! # Spans
//! Spans come from the TOML parser (`toml::de::DeTable::parse`, which keeps a
//! byte range for every key and value, and `toml::de::Error::span`). A
//! resolved configuration no longer knows which layer set a value, so
//! [`SourceIndex`] maps a field path back to the **highest-priority source
//! file that contains that path**. That is exact for values a single file
//! sets and approximate when a patch or a lower layer produced the final
//! value (the span then points at the most specific file that mentions the
//! path). A path no supplied file contains is reported without a location —
//! the field path is still set, so the diagnostic stays actionable.
//!
//! # Concurrency
//! All types are `Send + Sync`; every function is pure.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::DslError;
use crate::layers::DefinitionLayer;

/// Severity of a [`Diagnostic`].
///
/// # Description
/// `Error` stops lowering (the result is `Err(Diagnostics)`); `Warning` and
/// `Note` are carried in the IR trace and never change the IR itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The definition is invalid; no IR is produced.
    Error,
    /// The definition is valid, but something is likely a mistake.
    Warning,
    /// Purely informative.
    Note,
}

impl Severity {
    /// The lowercase label used in rendered diagnostics (`error`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Area of a diagnostic code (`HARW-<AREA>-NNN`, DSL §20.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Area {
    /// TOML syntax and wrongly typed values.
    Parse,
    /// The `schema` field, unknown tables or keys.
    Schema,
    /// IDs, versions, `extends`, mixins, cycles.
    Resolve,
    /// Patch paths and merge operators (§7).
    Patch,
    /// Authority monotonicity and ceilings.
    Auth,
    /// Role compatibility and the spawn matrix.
    Role,
    /// Tool surface.
    Tool,
    /// Context policy and program binding.
    Ctx,
    /// Return contracts and validators.
    Return,
    /// `[models]` and effort labels.
    Model,
    /// Skill references.
    Skill,
    /// `[binary]` and interface selection.
    Binary,
    /// Families, clans, organizations (reserved, no codes in this crate).
    Org,
    /// Compiler backends (reserved, no codes in this crate).
    Build,
}

impl Area {
    /// The upper-case area label inside a code (`PARSE`, `SCHEMA`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Area::Parse => "PARSE",
            Area::Schema => "SCHEMA",
            Area::Resolve => "RESOLVE",
            Area::Patch => "PATCH",
            Area::Auth => "AUTH",
            Area::Role => "ROLE",
            Area::Tool => "TOOL",
            Area::Ctx => "CTX",
            Area::Return => "RETURN",
            Area::Model => "MODEL",
            Area::Skill => "SKILL",
            Area::Binary => "BINARY",
            Area::Org => "ORG",
            Area::Build => "BUILD",
        }
    }
}

/// One entry of the diagnostic catalog.
///
/// # Description
/// `code` is the stable identifier, `severity` the default severity a
/// diagnostic with this code is created with, `title` a one-line summary and
/// `help` the default help text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticCode {
    /// Stable code, e.g. `"HARW-SCHEMA-002"`.
    pub code: &'static str,
    /// Area of the code.
    pub area: Area,
    /// Default severity.
    pub severity: Severity,
    /// One-line summary.
    pub title: &'static str,
    /// Default help text.
    pub help: &'static str,
}

/// All diagnostic codes of this crate, grouped by area.
///
/// # Description
/// Each constant is one [`DiagnosticCode`]. The numbering inside an area is
/// assigned here and never reused (DSL §20.1).
pub mod codes {
    use super::{Area, DiagnosticCode, Severity};

    macro_rules! code {
        ($name:ident, $code:literal, $area:ident, $sev:ident, $title:literal, $help:literal) => {
            #[doc = $title]
            pub const $name: DiagnosticCode = DiagnosticCode {
                code: $code,
                area: Area::$area,
                severity: Severity::$sev,
                title: $title,
                help: $help,
            };
        };
    }

    // ── PARSE ──────────────────────────────────────────────────────────
    code!(
        PARSE_SYNTAX,
        "HARW-PARSE-001",
        Parse,
        Error,
        "TOML syntax error",
        "fix the TOML syntax at the reported location"
    );
    code!(
        PARSE_STRUCTURE,
        "HARW-PARSE-002",
        Parse,
        Error,
        "the definition header does not deserialize (missing or wrongly typed required field)",
        "a definition needs `schema`, `id`, `version`, `role` and `specialization` with the documented types"
    );
    code!(
        PARSE_WRONG_TYPE,
        "HARW-PARSE-003",
        Parse,
        Error,
        "a value has the wrong TOML type",
        "use the type documented for this key; a wrongly typed value is an error, not an absent value"
    );
    code!(
        PARSE_OUT_OF_RANGE,
        "HARW-PARSE-004",
        Parse,
        Error,
        "an integer is negative or too large",
        "use a non-negative integer within the documented range"
    );

    // ── SCHEMA ─────────────────────────────────────────────────────────
    code!(
        SCHEMA_MISMATCH,
        "HARW-SCHEMA-001",
        Schema,
        Error,
        "unsupported `schema` value",
        "agent definitions use `schema = \"harwness.agent/v1\"`, mixins `harwness.mixin/v1`"
    );
    code!(
        SCHEMA_UNKNOWN_TABLE,
        "HARW-SCHEMA-002",
        Schema,
        Error,
        "unknown table in the definition",
        "remove the table or fix its name; unknown tables are never ignored"
    );
    code!(
        SCHEMA_UNLOWERED_TABLE,
        "HARW-SCHEMA-003",
        Schema,
        Warning,
        "known table that this version does not lower yet",
        "the table is documented in the DSL but has no effect yet; it is kept out of the IR"
    );
    code!(
        SCHEMA_UNKNOWN_KEY,
        "HARW-SCHEMA-004",
        Schema,
        Error,
        "unknown key in a known table",
        "remove the key or fix its name; the help lists the known keys"
    );
    code!(
        SCHEMA_ALIAS_CONFLICT,
        "HARW-SCHEMA-005",
        Schema,
        Error,
        "a table is given under both its name and its alias",
        "use one name only, e.g. `[spawn]` instead of `[spawn]` plus `[spawn_contract]`"
    );
    code!(
        SCHEMA_EMPTY_SPECIALIZATION,
        "HARW-SCHEMA-006",
        Schema,
        Error,
        "`specialization` is empty",
        "give the definition a non-empty specialization (DSL §10)"
    );
    code!(
        SCHEMA_INSTRUCTIONS,
        "HARW-SCHEMA-007",
        Schema,
        Error,
        "the instructions file cannot be loaded",
        "`instructions_file` is a relative path inside the agent directory that names a readable UTF-8 file"
    );

    // ── RESOLVE ────────────────────────────────────────────────────────
    code!(
        RESOLVE_MISSING_BASE,
        "HARW-RESOLVE-001",
        Resolve,
        Error,
        "the `extends` base definition does not exist",
        "check the base ID and version, and that the base is installed in a lower layer"
    );
    code!(
        RESOLVE_MISSING_MIXIN,
        "HARW-RESOLVE-002",
        Resolve,
        Error,
        "a mixin does not exist",
        "check the mixin ID and version"
    );
    code!(
        RESOLVE_CYCLE,
        "HARW-RESOLVE-003",
        Resolve,
        Error,
        "inheritance cycle",
        "break the cycle in the `extends` chain"
    );
    code!(
        RESOLVE_SHADOWED_TABLE,
        "HARW-RESOLVE-004",
        Resolve,
        Warning,
        "a table of this definition is shadowed by its base",
        "the base already defines this table, so the resolver keeps the base's table; use `[patch.<table>]` to change inherited values"
    );
    code!(
        RESOLVE_INVALID_ID,
        "HARW-RESOLVE-005",
        Resolve,
        Error,
        "invalid definition ID",
        "IDs have the form `<namespace>.<kind>.<name>@<major>`"
    );
    code!(
        RESOLVE_INVALID_VERSION,
        "HARW-RESOLVE-006",
        Resolve,
        Error,
        "invalid semantic version",
        "use a full semantic version such as `1.0.0`"
    );
    code!(
        RESOLVE_IO,
        "HARW-RESOLVE-007",
        Resolve,
        Error,
        "a definition file cannot be read",
        "check that the file exists and is readable"
    );

    // ── PATCH ──────────────────────────────────────────────────────────
    code!(
        PATCH_UNKNOWN_OP,
        "HARW-PATCH-001",
        Patch,
        Error,
        "unknown merge operator or malformed patch value",
        "known operators: replace, append, prepend, remove, intersect, min, max-within-parent"
    );
    code!(
        PATCH_MISSING_PATH,
        "HARW-PATCH-002",
        Patch,
        Error,
        "the patch path names no inherited field",
        "only `replace` may introduce a field; every other operator needs an inherited value"
    );
    code!(
        PATCH_TYPE_MISMATCH,
        "HARW-PATCH-003",
        Patch,
        Error,
        "merge operator applied to a value of the wrong type",
        "array operators need arrays, `min`/`max-within-parent` need numbers, nested paths need tables"
    );
    code!(
        PATCH_EXCEEDS_PARENT,
        "HARW-PATCH-004",
        Patch,
        Error,
        "`max-within-parent` asks for more than the parent allows",
        "request at most the inherited value, or use `min`"
    );
    code!(
        PATCH_AMBIGUOUS,
        "HARW-PATCH-005",
        Patch,
        Error,
        "a patch table mixes operators with nested keys",
        "split the patch: operators apply to this path, nested keys to paths below it"
    );

    // ── AUTH ───────────────────────────────────────────────────────────
    code!(
        AUTH_ELEVATION,
        "HARW-AUTH-001",
        Auth,
        Error,
        "authority elevation",
        "a definition may only keep or reduce the capabilities it inherits"
    );
    code!(
        AUTH_FORBIDDEN_OP,
        "HARW-AUTH-002",
        Auth,
        Error,
        "operator not allowed on an authority-bearing set",
        "authority capabilities accept only `intersect` and `remove`"
    );

    // ── ROLE ───────────────────────────────────────────────────────────
    code!(
        ROLE_MIXIN,
        "HARW-ROLE-001",
        Role,
        Error,
        "a mixin has a role incompatible with the definition",
        "mixins may not change the role (DSL §6)"
    );
    code!(
        ROLE_SPAWN_MATRIX,
        "HARW-ROLE-002",
        Role,
        Error,
        "the role may not spawn child orchestrators",
        "only orchestrator roles may list `spawn.child_orchestrators`"
    );

    // ── TOOL ───────────────────────────────────────────────────────────
    code!(
        TOOL_CONFLICT,
        "HARW-TOOL-001",
        Tool,
        Error,
        "a tool is both admitted and forbidden",
        "remove the tool from one of `tools.admitted` and `tools.forbidden`"
    );
    code!(
        TOOL_DUPLICATE,
        "HARW-TOOL-002",
        Tool,
        Warning,
        "a tool is listed twice",
        "remove the duplicate entry"
    );
    code!(
        TOOL_INVALID_NAME,
        "HARW-TOOL-003",
        Tool,
        Error,
        "invalid tool name",
        "tool names are non-empty and contain no whitespace or control characters"
    );

    // ── CTX ────────────────────────────────────────────────────────────
    code!(
        CTX_UNKNOWN_PROGRAM,
        "HARW-CTX-001",
        Ctx,
        Error,
        "unknown context program",
        "`[context] program` names a program of the context-program library"
    );
    code!(
        CTX_PROGRAM_RESOLUTION,
        "HARW-CTX-002",
        Ctx,
        Error,
        "the context program cannot be resolved",
        "fix the context program definition"
    );
    code!(
        CTX_DEFERRED_SECTIONS,
        "HARW-CTX-003",
        Ctx,
        Note,
        "must-include sections outside the root context ceiling are deferred",
        "these sections stay named by the program but are not required at child start"
    );

    // ── RETURN ─────────────────────────────────────────────────────────
    code!(
        RETURN_UNKNOWN_CONTRACT,
        "HARW-RETURN-001",
        Return,
        Error,
        "unknown return contract",
        "use one of the known return contracts; an unknown contract is not treated as free text"
    );
    code!(
        RETURN_INVALID_VALIDATOR,
        "HARW-RETURN-002",
        Return,
        Error,
        "invalid validator label",
        "validator labels are non-empty and contain no whitespace"
    );

    // ── MODEL ──────────────────────────────────────────────────────────
    code!(
        MODEL_UNKNOWN_EFFORT,
        "HARW-MODEL-001",
        Model,
        Error,
        "unknown effort label",
        "use one of: minimal, low, medium, high, xhigh, max"
    );
    code!(
        MODEL_INVALID_ENV,
        "HARW-MODEL-002",
        Model,
        Error,
        "invalid environment variable name",
        "environment variable names match `[A-Z_][A-Z0-9_]*`"
    );
    code!(
        MODEL_INVALID_FALLBACK,
        "HARW-MODEL-003",
        Model,
        Error,
        "malformed model fallback",
        "fallbacks have the form `provider/model`"
    );

    // ── SKILL ──────────────────────────────────────────────────────────
    code!(
        SKILL_INVALID,
        "HARW-SKILL-001",
        Skill,
        Error,
        "invalid skill entry",
        "skill names match `[a-z0-9-]{1,64}` and appear once"
    );

    // ── BINARY ─────────────────────────────────────────────────────────
    code!(
        BINARY_UNKNOWN_INTERFACE,
        "HARW-BINARY-001",
        Binary,
        Error,
        "unknown binary interface",
        "known interfaces: cli, repl, mcp, http, tui"
    );
    code!(
        BINARY_DEFAULT_NOT_LISTED,
        "HARW-BINARY-002",
        Binary,
        Error,
        "`default_interface` is not one of `interfaces`",
        "add the default interface to `interfaces` or choose a listed one"
    );
    code!(
        BINARY_EMPTY_INTERFACES,
        "HARW-BINARY-003",
        Binary,
        Error,
        "`interfaces` is empty",
        "list at least one interface or omit the key for the default `[\"cli\"]`"
    );
    code!(
        BINARY_INVALID_NAME,
        "HARW-BINARY-004",
        Binary,
        Error,
        "invalid binary name",
        "binary names match `[a-z0-9][a-z0-9._-]{0,63}`"
    );
    code!(
        BINARY_DUPLICATE_INTERFACE,
        "HARW-BINARY-005",
        Binary,
        Warning,
        "an interface is listed twice",
        "remove the duplicate entry"
    );
}

/// Every code of [`codes`], in catalog order.
pub const CATALOG: &[DiagnosticCode] = &[
    codes::PARSE_SYNTAX,
    codes::PARSE_STRUCTURE,
    codes::PARSE_WRONG_TYPE,
    codes::PARSE_OUT_OF_RANGE,
    codes::SCHEMA_MISMATCH,
    codes::SCHEMA_UNKNOWN_TABLE,
    codes::SCHEMA_UNLOWERED_TABLE,
    codes::SCHEMA_UNKNOWN_KEY,
    codes::SCHEMA_ALIAS_CONFLICT,
    codes::SCHEMA_EMPTY_SPECIALIZATION,
    codes::SCHEMA_INSTRUCTIONS,
    codes::RESOLVE_MISSING_BASE,
    codes::RESOLVE_MISSING_MIXIN,
    codes::RESOLVE_CYCLE,
    codes::RESOLVE_SHADOWED_TABLE,
    codes::RESOLVE_INVALID_ID,
    codes::RESOLVE_INVALID_VERSION,
    codes::RESOLVE_IO,
    codes::PATCH_UNKNOWN_OP,
    codes::PATCH_MISSING_PATH,
    codes::PATCH_TYPE_MISMATCH,
    codes::PATCH_EXCEEDS_PARENT,
    codes::PATCH_AMBIGUOUS,
    codes::AUTH_ELEVATION,
    codes::AUTH_FORBIDDEN_OP,
    codes::ROLE_MIXIN,
    codes::ROLE_SPAWN_MATRIX,
    codes::TOOL_CONFLICT,
    codes::TOOL_DUPLICATE,
    codes::TOOL_INVALID_NAME,
    codes::CTX_UNKNOWN_PROGRAM,
    codes::CTX_PROGRAM_RESOLUTION,
    codes::CTX_DEFERRED_SECTIONS,
    codes::RETURN_UNKNOWN_CONTRACT,
    codes::RETURN_INVALID_VALIDATOR,
    codes::MODEL_UNKNOWN_EFFORT,
    codes::MODEL_INVALID_ENV,
    codes::MODEL_INVALID_FALLBACK,
    codes::SKILL_INVALID,
    codes::BINARY_UNKNOWN_INTERFACE,
    codes::BINARY_DEFAULT_NOT_LISTED,
    codes::BINARY_EMPTY_INTERFACES,
    codes::BINARY_INVALID_NAME,
    codes::BINARY_DUPLICATE_INTERFACE,
];

/// Looks up a code in [`CATALOG`].
///
/// # Returns
/// The catalog entry, or `None` for a code this crate does not define.
#[must_use]
pub fn lookup(code: &str) -> Option<&'static DiagnosticCode> {
    CATALOG.iter().find(|entry| entry.code == code)
}

/// A location in a source file: 1-based line and column (in characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    /// File label as supplied by the caller (usually a path).
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column, counted in Unicode scalar values.
    pub column: u32,
}

impl SourceSpan {
    /// Converts a byte offset in `text` into a line/column span.
    ///
    /// # Description
    /// An offset past the end of `text` is clamped to the end; an offset in
    /// the middle of a multi-byte character counts that character.
    #[must_use]
    pub fn from_offset(file: impl Into<String>, text: &str, offset: usize) -> Self {
        let mut line: u32 = 1;
        let mut column: u32 = 1;
        for (index, ch) in text.char_indices() {
            if index >= offset {
                break;
            }
            if ch == '\n' {
                line = line.saturating_add(1);
                column = 1;
            } else {
                column = column.saturating_add(1);
            }
        }
        Self {
            file: file.into(),
            line,
            column,
        }
    }
}

impl fmt::Display for SourceSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// One diagnostic: code, severity, message, help, location and field path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// Stable code from [`CATALOG`], e.g. `"HARW-SCHEMA-002"`.
    pub code: String,
    /// Severity.
    pub severity: Severity,
    /// Human-readable message naming the offending value.
    pub message: String,
    /// Help text: how to fix the problem.
    pub help: Option<String>,
    /// Source location, if a supplied source file contains the path.
    pub span: Option<SourceSpan>,
    /// Field path inside the definition, e.g. `"tools.admitted"`.
    pub path: Option<String>,
}

impl Diagnostic {
    /// Creates a diagnostic with the catalog severity and help of `code`.
    #[must_use]
    pub fn new(code: &DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code.to_owned(),
            severity: code.severity,
            message: message.into(),
            help: Some(code.help.to_owned()),
            span: None,
            path: None,
        }
    }

    /// Replaces the help text.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Sets the field path.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Sets the source location.
    #[must_use]
    pub fn with_span(mut self, span: Option<SourceSpan>) -> Self {
        self.span = span;
        self
    }

    /// `true` for [`Severity::Error`].
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Maps a resolver/parser error to a diagnostic with a stable code.
    ///
    /// # Description
    /// The message is the error's `Display`; the field path comes from the
    /// error's [`crate::error::DiagLocation`], where it has one. Spans are
    /// attached by the caller ([`SourceIndex::span_for`]), because a
    /// `DslError` does not know the source files.
    #[must_use]
    pub fn from_dsl_error(error: &DslError) -> Self {
        let (code, path) = match error {
            DslError::Parse(_) => (&codes::PARSE_SYNTAX, None),
            DslError::Toml(_) => (&codes::PARSE_STRUCTURE, None),
            DslError::InvalidId { .. } => (&codes::RESOLVE_INVALID_ID, None),
            DslError::Semver(_) => (&codes::RESOLVE_INVALID_VERSION, None),
            DslError::Io { .. } => (&codes::RESOLVE_IO, None),
            DslError::MissingBase { location, .. } => {
                (&codes::RESOLVE_MISSING_BASE, location.field_path.clone())
            }
            DslError::MissingMixin { location, .. } => {
                (&codes::RESOLVE_MISSING_MIXIN, location.field_path.clone())
            }
            DslError::InheritanceCycle { location, .. } => {
                (&codes::RESOLVE_CYCLE, location.field_path.clone())
            }
            DslError::AuthorityElevation { location, .. } => {
                (&codes::AUTH_ELEVATION, location.field_path.clone())
            }
            DslError::AuthorityOpForbidden { location, .. } => {
                (&codes::AUTH_FORBIDDEN_OP, location.field_path.clone())
            }
            DslError::IllegalRoleForMixin { location, .. } => {
                (&codes::ROLE_MIXIN, location.field_path.clone())
            }
            DslError::InvalidSkill { location, .. } => {
                (&codes::SKILL_INVALID, location.field_path.clone())
            }
            DslError::UnknownMergeOp { .. } => (&codes::PATCH_UNKNOWN_OP, None),
            DslError::PatchPathMissing { location, .. } => {
                (&codes::PATCH_MISSING_PATH, location.field_path.clone())
            }
            DslError::PatchTypeMismatch { location, .. } => {
                (&codes::PATCH_TYPE_MISMATCH, location.field_path.clone())
            }
            DslError::PatchExceedsParent { location, .. } => {
                (&codes::PATCH_EXCEEDS_PARENT, location.field_path.clone())
            }
            DslError::PatchAmbiguous { location, .. } => {
                (&codes::PATCH_AMBIGUOUS, location.field_path.clone())
            }
            DslError::SchemaMismatch { location, .. } => {
                (&codes::SCHEMA_MISMATCH, location.field_path.clone())
            }
            DslError::UnknownReturnContract { .. } => (
                &codes::RETURN_UNKNOWN_CONTRACT,
                Some("return.contract".to_owned()),
            ),
            DslError::UnknownContextProgram { .. } => (
                &codes::CTX_UNKNOWN_PROGRAM,
                Some("context.program".to_owned()),
            ),
            DslError::ContextProgramResolution { .. } => (
                &codes::CTX_PROGRAM_RESOLUTION,
                Some("context.program".to_owned()),
            ),
        };
        let mut diagnostic = Diagnostic::new(code, error.to_string());
        diagnostic.path = path;
        diagnostic
    }
}

impl fmt::Display for Diagnostic {
    /// Renders the diagnostic in the DSL §20 format:
    ///
    /// ```text
    /// error[HARW-SCHEMA-002]: unknown table `[tols]`
    ///   --> agents/x/definition.toml:12:1
    ///   at: tols
    ///   help: …
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity, self.code, self.message)?;
        if let Some(span) = &self.span {
            write!(f, "\n  --> {span}")?;
        }
        if let Some(path) = &self.path {
            write!(f, "\n  at: {path}")?;
        }
        if let Some(help) = &self.help {
            write!(f, "\n  help: {help}")?;
        }
        Ok(())
    }
}

/// An ordered collection of [`Diagnostic`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// An empty collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one diagnostic.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    /// Appends every diagnostic of `other`.
    pub fn extend(&mut self, other: Diagnostics) {
        self.items.extend(other.items);
    }

    /// `true` if at least one diagnostic is an error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(Diagnostic::is_error)
    }

    /// Number of diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` if there is no diagnostic.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterates the diagnostics in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }

    /// The diagnostics as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[Diagnostic] {
        &self.items
    }

    /// Consumes the collection.
    #[must_use]
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }

    /// `true` if a diagnostic with `code` is present.
    #[must_use]
    pub fn contains_code(&self, code: &str) -> bool {
        self.items.iter().any(|diagnostic| diagnostic.code == code)
    }

    /// All codes in insertion order (duplicates kept).
    #[must_use]
    pub fn codes(&self) -> Vec<&str> {
        self.items
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect()
    }
}

impl From<Vec<Diagnostic>> for Diagnostics {
    fn from(items: Vec<Diagnostic>) -> Self {
        Self { items }
    }
}

impl From<Diagnostic> for Diagnostics {
    fn from(item: Diagnostic) -> Self {
        Self { items: vec![item] }
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.items.iter().enumerate() {
            if index > 0 {
                f.write_str("\n\n")?;
            }
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}

/// One source file of a definition layer, as supplied by the caller.
///
/// # Description
/// `path` is the label used in spans (and the directory against which an
/// `instructions_file` is resolved); `text` the TOML source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Layer this file belongs to.
    pub layer: DefinitionLayer,
    /// Path of the file (used for spans and relative instruction files).
    pub path: std::path::PathBuf,
    /// TOML source text.
    pub text: String,
}

impl SourceFile {
    /// Creates a source file entry.
    #[must_use]
    pub fn new(
        layer: DefinitionLayer,
        path: impl Into<std::path::PathBuf>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            layer,
            path: path.into(),
            text: text.into(),
        }
    }

    /// The label used in spans.
    #[must_use]
    pub fn label(&self) -> String {
        self.path.display().to_string()
    }

    /// The top-level `id` string of this file, if it parses and has one.
    #[must_use]
    pub fn declared_id(&self) -> Option<String> {
        let parsed = toml::de::DeTable::parse(&self.text).ok()?;
        let value = parsed.get_ref().get("id")?;
        value.get_ref().as_str().map(str::to_owned)
    }

    /// The span of the value at `path` (`"tools.admitted"`, `"mixins[0]"`),
    /// if this file contains it.
    #[must_use]
    pub fn span_of(&self, path: &str) -> Option<SourceSpan> {
        let parsed = toml::de::DeTable::parse(&self.text).ok()?;
        let offset = offset_of_path(parsed.get_ref(), path)?;
        Some(SourceSpan::from_offset(self.label(), &self.text, offset))
    }
}

/// Parsed path segment: a key, optionally followed by array indices.
fn split_path(path: &str) -> Vec<(&str, Vec<usize>)> {
    path.split('.')
        .map(|segment| {
            let mut parts = segment.split('[');
            let key = parts.next().unwrap_or_default();
            let indices = parts
                .filter_map(|index| index.trim_end_matches(']').parse::<usize>().ok())
                .collect();
            (key, indices)
        })
        .collect()
}

/// Byte offset of the value at `path` inside a parsed document.
fn offset_of_path(root: &toml::de::DeTable<'_>, path: &str) -> Option<usize> {
    let segments = split_path(path);
    let mut table = root;
    let mut offset = None;
    let last = segments.len().checked_sub(1)?;
    for (position, (key, indices)) in segments.iter().enumerate() {
        let mut value = table.get(*key)?;
        for index in indices {
            let array = value.get_ref().as_array()?;
            value = array.get(*index)?;
        }
        offset = Some(value.span().start);
        if position < last {
            table = value.get_ref().as_table()?;
        }
    }
    offset
}

/// Maps field paths back to source files (§ module docs, "Spans").
///
/// # Description
/// Holds the caller's source files. [`Self::span_for`] searches the files
/// that declare `target_id` first (highest layer first), then every other
/// file (highest layer first), and returns the span of the first file that
/// contains the path.
#[derive(Debug, Clone, Copy)]
pub struct SourceIndex<'a> {
    files: &'a [SourceFile],
    target_id: Option<&'a str>,
}

impl<'a> SourceIndex<'a> {
    /// Creates an index over `files`, preferring files that declare `target_id`.
    #[must_use]
    pub fn new(files: &'a [SourceFile], target_id: Option<&'a str>) -> Self {
        Self { files, target_id }
    }

    /// Files in lookup order: target files (highest layer first), then the rest.
    fn ordered(&self) -> Vec<&'a SourceFile> {
        let mut ordered: Vec<(bool, usize, &'a SourceFile)> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let is_target = match self.target_id {
                    Some(target) => file.declared_id().as_deref() == Some(target),
                    None => false,
                };
                (is_target, index, file)
            })
            .collect();
        // Target files first; inside each group higher layer first, and for
        // equal layers the later file (the caller's order) first.
        ordered.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.2.layer.cmp(&left.2.layer))
                .then_with(|| right.1.cmp(&left.1))
        });
        ordered.into_iter().map(|(_, _, file)| file).collect()
    }

    /// The span of `path` in the most specific file that contains it.
    #[must_use]
    pub fn span_for(&self, path: &str) -> Option<SourceSpan> {
        self.ordered()
            .into_iter()
            .find_map(|file| file.span_of(path))
    }

    /// The files declaring the target ID, highest layer first.
    #[must_use]
    pub fn target_files(&self) -> Vec<&'a SourceFile> {
        let Some(target) = self.target_id else {
            return Vec::new();
        };
        let mut files: Vec<(usize, &'a SourceFile)> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.declared_id().as_deref() == Some(target))
            .collect();
        files.sort_by(|left, right| {
            right
                .1
                .layer
                .cmp(&left.1.layer)
                .then_with(|| right.0.cmp(&left.0))
        });
        files.into_iter().map(|(_, file)| file).collect()
    }
}

/// Parses one definition file into a [`crate::raw::RawAgentDefinition`],
/// reporting failures as diagnostics with spans.
///
/// # Description
/// A TOML syntax error becomes `HARW-PARSE-001`; a document that is valid
/// TOML but does not deserialize (missing `role`, wrongly typed `skills`, …)
/// becomes `HARW-PARSE-002`. Both carry the parser's span.
///
/// # Errors
/// [`Diagnostics`] with exactly one error.
pub fn parse_source(file: &SourceFile) -> Result<crate::raw::RawAgentDefinition, Diagnostics> {
    let span_of = |error: &toml::de::Error| {
        error
            .span()
            .map(|range| SourceSpan::from_offset(file.label(), &file.text, range.start))
    };
    if let Err(error) = toml::de::DeTable::parse(&file.text) {
        return Err(
            Diagnostic::new(&codes::PARSE_SYNTAX, error.message().to_owned())
                .with_span(span_of(&error))
                .into(),
        );
    }
    toml::from_str::<crate::raw::RawAgentDefinition>(&file.text).map_err(|error| {
        Diagnostic::new(&codes::PARSE_STRUCTURE, error.message().to_owned())
            .with_span(span_of(&error))
            .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_catalog_codes_are_unique_and_well_formed() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in CATALOG {
            assert!(seen.insert(entry.code), "duplicate code {}", entry.code);
            let expected_prefix = format!("HARW-{}-", entry.area.as_str());
            assert!(
                entry.code.starts_with(&expected_prefix),
                "{} does not match its area {}",
                entry.code,
                entry.area.as_str()
            );
            let number = &entry.code[expected_prefix.len()..];
            assert_eq!(number.len(), 3, "{}", entry.code);
            assert!(number.chars().all(|c| c.is_ascii_digit()), "{}", entry.code);
            assert!(!entry.title.is_empty() && !entry.help.is_empty());
        }
    }

    #[test]
    fn test_reserved_areas_have_no_codes_yet() {
        assert!(
            CATALOG
                .iter()
                .all(|entry| entry.area != Area::Org && entry.area != Area::Build)
        );
    }

    #[test]
    fn test_lookup_finds_catalog_entries() {
        assert_eq!(
            lookup("HARW-SCHEMA-002").map(|entry| entry.severity),
            Some(Severity::Error)
        );
        assert!(lookup("HARW-NOPE-001").is_none());
    }

    #[test]
    fn test_span_from_offset_counts_lines_and_columns() {
        let text = "a = 1\nbb = 2\n";
        let span = SourceSpan::from_offset("f.toml", text, 7);
        assert_eq!((span.line, span.column), (2, 2));
        assert_eq!(span.to_string(), "f.toml:2:2");
    }

    #[test]
    fn test_source_file_span_of_nested_path_and_index() -> TestResult {
        let file = SourceFile::new(
            DefinitionLayer::BuiltIn,
            "agents/x.toml",
            "id = \"harwness.agent.x@1\"\n\n[tools]\nadmitted = [\"fs.read\", \"fs.list\"]\n",
        );
        let span = file
            .span_of("tools.admitted[1]")
            .ok_or(TestError::Missing("span"))?;
        assert_eq!(span.line, 4);
        assert_eq!(span.column, 24);
        assert!(file.span_of("tools.forbidden").is_none());
        assert_eq!(file.declared_id().as_deref(), Some("harwness.agent.x@1"));
        Ok(())
    }

    #[test]
    fn test_source_index_prefers_the_target_file_of_the_highest_layer() -> TestResult {
        let files = vec![
            SourceFile::new(
                DefinitionLayer::BuiltIn,
                "base.toml",
                "id = \"harwness.agent.base@1\"\n[tools]\nadmitted = []\n",
            ),
            SourceFile::new(
                DefinitionLayer::BuiltIn,
                "low.toml",
                "id = \"harwness.agent.t@1\"\n[tools]\nadmitted = []\n",
            ),
            SourceFile::new(
                DefinitionLayer::Project,
                "high.toml",
                "id = \"harwness.agent.t@1\"\n\n\n[tools]\nadmitted = []\n",
            ),
        ];
        let index = SourceIndex::new(&files, Some("harwness.agent.t@1"));
        let span = index
            .span_for("tools.admitted")
            .ok_or(TestError::Missing("span"))?;
        assert_eq!(span.file, "high.toml");
        assert_eq!(index.target_files().len(), 2);
        Ok(())
    }

    #[test]
    fn test_diagnostic_display_has_code_location_path_and_help() {
        let diagnostic = Diagnostic::new(&codes::SCHEMA_UNKNOWN_TABLE, "unknown table `[tols]`")
            .with_path("tols")
            .with_span(Some(SourceSpan {
                file: "a.toml".to_owned(),
                line: 3,
                column: 1,
            }));
        let rendered = diagnostic.to_string();
        assert!(rendered.starts_with("error[HARW-SCHEMA-002]: unknown table"));
        assert!(rendered.contains("--> a.toml:3:1"));
        assert!(rendered.contains("at: tols"));
        assert!(rendered.contains("help: "));
    }

    #[test]
    fn test_diagnostics_serde_roundtrip_and_unknown_field_rejected() -> TestResult {
        let diagnostics: Diagnostics =
            Diagnostic::new(&codes::TOOL_DUPLICATE, "fs.read twice").into();
        let json = serde_json::to_string(&diagnostics)?;
        let back: Diagnostics = serde_json::from_str(&json)?;
        assert_eq!(back, diagnostics);
        let bad = r#"{"code":"X","severity":"error","message":"m","help":null,"span":null,"path":null,"extra":1}"#;
        assert!(serde_json::from_str::<Diagnostic>(bad).is_err());
        Ok(())
    }

    #[test]
    fn test_parse_source_reports_syntax_and_structure_with_spans() -> TestResult {
        let syntax = SourceFile::new(DefinitionLayer::BuiltIn, "s.toml", "schema = [unclosed");
        let Err(diagnostics) = parse_source(&syntax) else {
            return Err(TestError::Unexpected("syntax error expected".to_owned()));
        };
        assert!(diagnostics.contains_code("HARW-PARSE-001"));
        assert!(diagnostics.iter().all(|d| d.span.is_some()));

        let structure = SourceFile::new(
            DefinitionLayer::BuiltIn,
            "m.toml",
            "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.m@1\"\nversion = \"1.0.0\"\nspecialization = \"m\"\n",
        );
        let result = parse_source(&structure);
        assert!(matches!(result, Err(ref d) if d.contains_code("HARW-PARSE-002")));
        Ok(())
    }
}
