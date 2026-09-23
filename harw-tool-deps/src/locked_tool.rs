//! `deps.locked` — die in `Cargo.lock` gesperrten Dependency-Versionen.
//!
//! # Verantwortung
//! Dieses Modul beantwortet die Frage "welche Version einer Dependency baut
//! dieses Projekt tatsächlich?" — die Voraussetzung dafür, dass
//! `deps.source_read` den *richtigen* Quellcode zitiert. Ohne `crate_name`
//! liefert es eine kompakte Übersicht, mit `crate_name` den exakten Eintrag
//! samt `source`/`checksum`.
//!
//! # Sicherheitskontrakt
//! - Permission: [`harw_authority::Permission::ReadWorkspace`] — `Cargo.lock`
//!   liegt im Workspace und wird ausschließlich über `canonical_root()` der
//!   Sandbox gelesen; es gibt kein Pfad-Argument.
//! - Die **Registry-Anreicherung** (aufgelöster Quellpfad, verfügbare
//!   Versionen) liegt außerhalb des Workspace. Sie wird deshalb nur ergänzt,
//!   wenn die Sandbox zusätzlich
//!   [`harw_authority::Permission::ReadCargoRegistry`] gewährt; sonst bleiben die
//!   Felder leer und die Antwort sagt das ausdrücklich (fail closed).
//!
//! # Schlüsseltypen
//! - [`LockedSummary`] — Name/Version-Paar der Übersichtsantwort.
//! - [`DepsLockedTool`] — der von `#[harw_macros::tool]` erzeugte Executor.
//!
//! # Fehler
//! [`crate::DepsToolError`]; im Tool wird jeder Fehler zu `Ok(ToolOutput::Error)`.
//!
//! # Nebenläufigkeit
//! Zustandslos und rein lesend; das Tool ist `parallel_safe`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_tool_deps::locked_tool::external_summary;
//! use harw_code_graph::parse_lockfile;
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
//! let packages = parse_lockfile(Path::new("."))?;
//! let (shown, total) = external_summary(&packages);
//! println!("{} von {total} externen Paketen", shown.len());
//! # Ok(())
//! # }
//! ```

use harw_code_graph::{LockedPackage, find_locked, parse_lockfile};
use harw_tools::{Permission, ToolOutput, ToolsError, executor::ToolExecutionContext};
use serde::{Deserialize, Serialize};

use crate::error::DepsToolError;
use crate::source_tool::{RegistryAccess, error_output, json_output};

/// Maximale Anzahl Pakete in der Übersichtsantwort ohne `crate_name`.
pub const MAX_LOCKED_ENTRIES: usize = 200;

/// Ein Paket der kompakten Übersichtsantwort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockedSummary {
    /// Crate-Name.
    pub name: String,
    /// Gesperrte Version.
    pub version: String,
}

/// Die Übersichtsantwort ohne `crate_name`.
///
/// # Description
/// `total_external` ist die **ungekappte** Gesamtzahl; `truncated` sagt, ob
/// `packages` gekürzt wurde. Beides gehört ins Ergebnis, damit ein Agent eine
/// gekappte Liste nicht für vollständig hält.
#[derive(Debug, Clone, Serialize)]
pub struct LockedOverview {
    /// Anzahl aller externen Pakete im Lockfile, vor der Kappung.
    pub total_external: usize,
    /// `true`, wenn `packages` auf [`MAX_LOCKED_ENTRIES`] gekürzt wurde.
    pub truncated: bool,
    /// Die (gekappten) Pakete, sortiert nach Name.
    pub packages: Vec<LockedSummary>,
}

/// Argumente für `deps.locked`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct DepsLockedArgs {
    /// Crate-Name für den Detaileintrag. Ohne Angabe wird eine Übersicht geliefert.
    pub crate_name: Option<String>,
}

/// Filtert die externen (Registry-)Pakete und kappt sie auf
/// [`MAX_LOCKED_ENTRIES`].
///
/// # Description
/// Externe Pakete sind genau die mit gesetztem `source`; Path-Dependencies des
/// eigenen Workspace haben keines und gehören in `deps.graph`, nicht hierher.
///
/// # Arguments
/// - `packages` (`&[LockedPackage]`): alle Einträge aus `Cargo.lock`.
///
/// # Returns
/// Ein `(Vec<LockedSummary>, usize)`-Paar: die gekappte, nach Name sortierte
/// Liste und die **ungekappte** Gesamtzahl externer Pakete.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Reine Funktion; sicher aus mehreren Threads.
///
/// # Examples
/// ```rust,no_run
/// use harw_code_graph::parse_lockfile;
/// use harw_tool_deps::locked_tool::external_summary;
/// use std::path::Path;
///
/// # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
/// let packages = parse_lockfile(Path::new("."))?;
/// let (shown, total) = external_summary(&packages);
/// assert!(shown.len() <= total);
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn external_summary(packages: &[LockedPackage]) -> (Vec<LockedSummary>, usize) {
    let mut external: Vec<LockedSummary> = packages
        .iter()
        .filter(|package| package.source.is_some())
        .map(|package| LockedSummary {
            name: package.name.clone(),
            version: package.version.clone(),
        })
        .collect();
    external.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.version.cmp(&right.version))
    });
    let total = external.len();
    external.truncate(MAX_LOCKED_ENTRIES);
    (external, total)
}

/// Baut den Detaileintrag zu einem einzelnen Crate.
///
/// # Description
/// Enthält immer `name`, `version`, `source` und `checksum` aus dem Lockfile.
/// `source_path` und `available_versions` kommen aus dem lokalen
/// Registry-Cache und werden nur gefüllt, wenn `access` übergeben wurde — der
/// Aufrufer entscheidet das anhand der `ReadCargoRegistry`-Permission.
///
/// # Arguments
/// - `package` (`&LockedPackage`): der gefundene Lockfile-Eintrag.
/// - `access` (`Option<&RegistryAccess>`): `None`, wenn der Registry-Blick
///   nicht autorisiert ist.
///
/// # Returns
/// Ein [`serde_json::Value`] mit `registry_consulted`, das ehrlich anzeigt, ob
/// der Cache befragt wurde.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Liest bei gesetztem `access` das Registry-Verzeichnis; ansonsten rein.
#[must_use]
pub fn locked_detail(
    package: &LockedPackage,
    access: Option<&RegistryAccess>,
) -> serde_json::Value {
    let mut source_path: Option<String> = None;
    let mut available_versions: Vec<String> = Vec::new();

    if let Some(access) = access {
        // Ein fehlender Cache-Eintrag ist kein Fehler: die Antwort bleibt
        // gültig, nur ohne Quellpfad.
        if let Ok(path) = access.locator().resolve(&package.name, &package.version) {
            source_path = Some(path.display().to_string());
        }
        if let Ok(versions) = access.locator().available_versions(&package.name) {
            available_versions = versions;
        }
    }

    serde_json::json!({
        "name": package.name,
        "version": package.version,
        "source": package.source,
        "checksum": package.checksum,
        "registry_consulted": access.is_some(),
        "source_path": source_path,
        "available_versions": available_versions,
    })
}

/// Liest `Cargo.lock` der Workspace-Wurzel und beantwortet Übersicht oder Detail.
///
/// # Description
/// Siehe [`external_summary`] und [`locked_detail`]. Die Wurzel stammt
/// ausschließlich aus `canonical_root()` der Sandbox; es gibt bewusst kein
/// Pfad-Argument.
///
/// # Errors
/// Liefert nie `Err`; jeder Fehler wird als `Ok(ToolOutput::Error)` gemeldet.
#[harw_macros::tool(
    name = "deps.locked",
    description = "Liest die Cargo.lock des Workspace. Ohne 'crate_name' liefert es eine \
                   kompakte Liste aller externen Pakete (Name + Version), auf 200 Einträge \
                   gekappt. Mit 'crate_name' liefert es den exakten Eintrag inklusive 'source' \
                   und 'checksum' und — falls die Sandbox zusätzlich den Registry-Lesezugriff \
                   gewährt — den aufgelösten lokalen Quellpfad sowie alle im Cache verfügbaren \
                   Versionen. Das ist der Weg von einem Crate-Namen zur exakt gebauten Version, \
                   bevor deps.source_read den Quellcode liest. Nur lesend.",
    permission = "read_workspace",
    parallel_safe
)]
async fn deps_locked(
    context: &ToolExecutionContext,
    args: DepsLockedArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "deps.locked";

    let root = context.sandbox().workspace().canonical_root();
    let packages = match parse_lockfile(root) {
        Ok(packages) => packages,
        Err(error) => return Ok(error_output(TOOL, &DepsToolError::CodeGraph(error))),
    };

    let Some(crate_name) = args.crate_name.as_deref() else {
        let (shown, total) = external_summary(&packages);
        tracing::info!(total, shown = shown.len(), "deps.locked Übersicht");
        let overview = LockedOverview {
            total_external: total,
            truncated: total > shown.len(),
            packages: shown,
        };
        return Ok(json_output(TOOL, &overview));
    };

    let Some(package) = find_locked(&packages, crate_name) else {
        return Ok(error_output(
            TOOL,
            &DepsToolError::CrateNotLocked {
                crate_name: crate_name.to_owned(),
            },
        ));
    };

    // Registry-Blick nur mit ausdrücklicher Berechtigung — er verlässt den Workspace.
    let access = if context
        .sandbox()
        .permissions()
        .contains(Permission::ReadCargoRegistry)
    {
        match RegistryAccess::from_env() {
            Ok(access) => Some(access),
            Err(error) => {
                tracing::warn!(error = %error, "Registry-Wurzel nicht auflösbar, Antwort ohne Quellpfad");
                None
            }
        }
    } else {
        None
    };

    tracing::debug!(
        crate_name,
        version = package.version.as_str(),
        registry_consulted = access.is_some(),
        "deps.locked Detail"
    );

    Ok(json_output(TOOL, &locked_detail(package, access.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        TestError, TestResult, block_on, ctx, sandbox_context, scratch_dir, tool_call,
        write_lockfile,
    };
    use harw_tools::ToolExecutor as _;
    use std::fs;

    fn sample_packages() -> Vec<LockedPackage> {
        vec![
            LockedPackage {
                name: "serde".to_owned(),
                version: "1.0.228".to_owned(),
                source: Some("registry+https://example.invalid/index".to_owned()),
                checksum: Some("abc123".to_owned()),
            },
            LockedPackage {
                name: "harw-types".to_owned(),
                version: "0.2.0".to_owned(),
                source: None,
                checksum: None,
            },
        ]
    }

    #[test]
    fn test_external_summary_filters_path_dependencies() {
        let (shown, total) = external_summary(&sample_packages());

        assert_eq!(total, 1, "nur Pakete mit 'source' sind extern");
        assert_eq!(
            shown,
            vec![LockedSummary {
                name: "serde".to_owned(),
                version: "1.0.228".to_owned(),
            }]
        );
    }

    #[test]
    fn test_external_summary_caps_at_two_hundred_entries() {
        let packages: Vec<LockedPackage> = (0..250)
            .map(|index| LockedPackage {
                name: format!("crate-{index:03}"),
                version: "1.0.0".to_owned(),
                source: Some("registry+https://example.invalid/index".to_owned()),
                checksum: None,
            })
            .collect();

        let (shown, total) = external_summary(&packages);

        assert_eq!(total, 250, "die Gesamtzahl bleibt ungekappt sichtbar");
        assert_eq!(shown.len(), MAX_LOCKED_ENTRIES);
        assert_eq!(shown[0].name, "crate-000", "sortiert nach Name");
    }

    #[test]
    fn test_locked_detail_without_registry_access_reports_it() {
        let packages = sample_packages();
        let detail = locked_detail(&packages[0], None);

        assert_eq!(detail["name"], "serde");
        assert_eq!(detail["checksum"], "abc123");
        assert_eq!(
            detail["registry_consulted"], false,
            "ohne Registry-Permission muss die Antwort das sagen"
        );
        assert!(detail["source_path"].is_null());
        assert_eq!(
            detail["available_versions"].as_array().map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn test_locked_detail_with_registry_access_resolves_source_path() -> TestResult {
        let cargo_home = scratch_dir("locked-registry")?;
        let crate_dir = cargo_home
            .join("registry")
            .join("src")
            .join("index.crates.io-testhash")
            .join("serde-1.0.228");
        fs::create_dir_all(&crate_dir).map_err(ctx("Registry-Fixture anlegen"))?;
        let access = RegistryAccess::with_home(cargo_home.clone());

        let packages = sample_packages();
        let detail = locked_detail(&packages[0], Some(&access));

        assert_eq!(detail["registry_consulted"], true);
        assert_eq!(detail["source_path"], crate_dir.display().to_string());
        assert_eq!(detail["available_versions"][0], "1.0.228");

        fs::remove_dir_all(&cargo_home).ok();
        Ok(())
    }

    #[test]
    fn test_deps_locked_tool_lists_external_packages() -> TestResult {
        let harness = scratch_dir("locked-tool-list")?;
        let workspace = harness.join("ws");
        fs::create_dir_all(&workspace).map_err(ctx("Workspace anlegen"))?;
        write_lockfile(&workspace)?;

        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace])?;
        let call = tool_call("deps.locked", serde_json::json!({}));

        let output =
            block_on(DepsLockedTool.execute(&context, &call))?.map_err(ctx("Tool läuft"))?;

        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["total_external"], 1);
                assert_eq!(content["truncated"], false);
                assert_eq!(content["packages"][0]["name"], "serde");
                assert_eq!(content["packages"][0]["version"], "1.0.228");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "JSON-Ausgabe erwartet, war: {other:?}"
                )));
            }
        }

        fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    #[test]
    fn test_deps_locked_tool_returns_detail_for_named_crate() -> TestResult {
        let harness = scratch_dir("locked-tool-detail")?;
        let workspace = harness.join("ws");
        fs::create_dir_all(&workspace).map_err(ctx("Workspace anlegen"))?;
        write_lockfile(&workspace)?;

        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace])?;
        let call = tool_call("deps.locked", serde_json::json!({ "crate_name": "serde" }));

        let output =
            block_on(DepsLockedTool.execute(&context, &call))?.map_err(ctx("Tool läuft"))?;

        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["version"], "1.0.228");
                assert_eq!(content["checksum"], "abc123");
                assert_eq!(
                    content["registry_consulted"], false,
                    "ohne ReadCargoRegistry darf der Cache nicht befragt werden"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "JSON-Ausgabe erwartet, war: {other:?}"
                )));
            }
        }

        fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    #[test]
    fn test_deps_locked_tool_reports_unknown_crate() -> TestResult {
        let harness = scratch_dir("locked-tool-unknown")?;
        let workspace = harness.join("ws");
        fs::create_dir_all(&workspace).map_err(ctx("Workspace anlegen"))?;
        write_lockfile(&workspace)?;

        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace])?;
        let call = tool_call(
            "deps.locked",
            serde_json::json!({ "crate_name": "gibt-es-nicht" }),
        );

        let output =
            block_on(DepsLockedTool.execute(&context, &call))?.map_err(ctx("Tool läuft"))?;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("gibt-es-nicht"), "war: {message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Fehlerausgabe erwartet, war: {other:?}"
                )));
            }
        }

        fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    #[test]
    fn test_deps_locked_tool_denies_without_read_workspace() -> TestResult {
        let harness = scratch_dir("locked-tool-denied")?;
        let workspace = harness.join("ws");
        fs::create_dir_all(&workspace).map_err(ctx("Workspace anlegen"))?;
        write_lockfile(&workspace)?;

        let context = sandbox_context(&harness, Vec::new())?;
        let call = tool_call("deps.locked", serde_json::json!({}));

        let output =
            block_on(DepsLockedTool.execute(&context, &call))?.map_err(ctx("Tool läuft"))?;

        match output {
            ToolOutput::Error { message } => assert!(
                message.contains("ReadWorkspace"),
                "fehlende Permission muss benannt werden, war: {message}"
            ),
            other => {
                return Err(TestError::Unexpected(format!(
                    "Fehlerausgabe erwartet, war: {other:?}"
                )));
            }
        }

        fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    #[test]
    fn test_deps_locked_tool_spec_advertises_its_name() {
        assert_eq!(DepsLockedTool::NAME, "deps.locked");
        assert_eq!(DepsLockedTool::spec().name(), "deps.locked");
        assert_eq!(DepsLockedTool::PERMISSION, Some(Permission::ReadWorkspace));
    }

    // `PARALLEL_SAFE` is a macro-generated `const bool` (see
    // `#[harw_macros::tool]`), so any `assert!` on it is compile-time-constant
    // by construction — exactly what clippy's `assertions_on_constants` flags
    // as checking nothing at runtime. A `const` assertion embraces that fact
    // instead of fighting it: it fails to *compile* the moment `DepsLockedTool`
    // stops declaring itself parallel-safe (other tools in the workspace do
    // declare `parallel_safe = false`, see harw-tools/src/provider_macro.rs,
    // so this is a real per-tool fact, not a type-level tautology), which is a
    // strictly stronger guarantee than the runtime assertion it replaces.
    const _: () = assert!(DepsLockedTool::PARALLEL_SAFE);
}
