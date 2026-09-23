//! `/add-workdir` — zusätzliche Arbeitsverzeichnisse für die Sitzung freigeben.
//!
//! Spec-Quelle: Contract `harw-scopes-contract.md` §2 (Slice A8,
//! `harw_sandbox::ExtraRootsCell`) und Schritt 6 des Plans
//! `nope-permissions-gibt-es-wild-lobster.md` (Slice B4).
//!
//! # Verantwortung
//! Diese Operation validiert einen Verzeichnis-Kandidaten über
//! [`harw_sandbox::validate_extra_root`] (via [`ExtraRootsCell::add`]) und
//! registriert ihn in der geteilten [`ExtraRootsCell`] der Sitzung. Sie
//! erweitert **nie** implizit eine Sandbox: die Zelle wird nur dann wirksam,
//! wenn eine `SandboxSpec` sie ausdrücklich über `with_extra_roots` gebunden
//! hat — das ist Sache der Kompositionswurzel (`harw-runtime`), nicht dieser
//! Operation.
//!
//! # Unterkommandos
//! - `/add-workdir` (kein Argument) — listet die aktuell registrierten
//!   zusätzlichen Wurzeln.
//! - `/add-workdir <pfad>` — validiert und registriert `<pfad>` für diese
//!   Sitzung.
//! - `/add-workdir <pfad> --save` — wie oben, und merkt `<pfad>` zusätzlich
//!   dauerhaft im Projekt-Scope (`[permissions] extra_roots` in
//!   `~/.harw/profiles/<profil>/projects/<schlüssel>/settings.toml`, siehe
//!   `crate::permissions::project_config_path`). Es gibt bewusst keinen
//!   `--global`-Scope für Arbeitsverzeichnisse — sie sind projektbezogen.
//! - `/add-workdir --remove <pfad>` — entfernt `<pfad>` aus der Sitzungs-
//!   Zelle und (bestes Bemühen) aus der Projekt-Datei.
//!
//! # Fehlermeldungen
//! [`harw_sandbox::ExtraRootError`] liefert englische Klartext-Begründungen
//! (`/` abgelehnt, `$HOME` abgelehnt, Vorfahre des Projekt-Roots, maximal 8
//! Wurzeln). Diese Operation reicht sie grundsätzlich unverändert als
//! [`OpError::InvalidArguments`] durch, übersetzt aber
//! [`harw_sandbox::ExtraRootError::UserHome`] in eine deutsche Meldung, die
//! ausdrücklich das Home-Verzeichnis nennt — der wichtigste der abgelehnten
//! Fälle, weil ein zu weit gefasster zusätzlicher Root hier am ehesten
//! versehentlich (statt bewusst) angefragt wird.

use std::path::PathBuf;

use harw_config::{ConfigWriter, SettingScope};
use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_sandbox::{ExtraRootError, ExtraRootsCell};

/// Meldung für den Fall, dass keine [`ExtraRootsCell`] registriert ist.
pub(crate) const NO_EXTRA_ROOTS_CELL: &str = crate::permissions::NO_EXTRA_ROOTS_CELL;

/// Argumente für `/add-workdir`.
///
/// # Beschreibung
/// Rohe Tokens; die Operation unterscheidet `--remove <pfad>`, `--save` und
/// den positionalen Pfad selbst in ihrem Rumpf (siehe Moduldoku).
#[derive(Default, serde::Deserialize)]
pub struct AddWorkdirArgs {
    /// Alle Tokens nach `/add-workdir`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl FromRawArgs for AddWorkdirArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Listet, fügt hinzu oder entfernt zusätzliche Arbeitsverzeichnisse dieser
/// Sitzung.
#[operation(
    name = "add-workdir",
    summary = "Gibt ein zusätzliches Arbeitsverzeichnis für diese Sitzung frei (optional dauerhaft fürs Projekt).",
    domain = "catalog_config",
    permission = "operator",
    command(path = "/add-workdir", visibility = "tui_only")
)]
async fn add_workdir(ctx: &OpContext, args: AddWorkdirArgs) -> Result<OpOutput, OpError> {
    if let Some(index) = args.tokens.iter().position(|token| token == "--remove") {
        let Some(path_str) = args.tokens.get(index + 1) else {
            return Err(OpError::InvalidArguments(
                "/add-workdir --remove <pfad> braucht einen Pfad".to_owned(),
            ));
        };
        return remove_workdir(ctx, path_str);
    }

    let save = args.tokens.iter().any(|token| token == "--save");
    let positional: Vec<&String> = args
        .tokens
        .iter()
        .filter(|token| token.as_str() != "--save")
        .collect();

    match positional.first() {
        None => list_workdirs(ctx),
        Some(path_str) => add_one_workdir(ctx, path_str, save),
    }
}

/// Listet alle aktuell registrierten zusätzlichen Wurzeln.
fn list_workdirs(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let Some(cell) = ctx.service::<ExtraRootsCell>() else {
        return Err(OpError::NotAvailable(NO_EXTRA_ROOTS_CELL.to_owned()));
    };
    let roots = cell.snapshot();
    if roots.is_empty() {
        return Ok(OpOutput::from(
            "Keine zusätzlichen Arbeitsverzeichnisse registriert.".to_owned(),
        ));
    }
    let mut buf = format!("{} zusätzliche(s) Arbeitsverzeichnis(se):\n", roots.len());
    for root in &roots {
        buf.push_str(&format!(
            "- {}{}\n",
            root.path.display(),
            if root.persisted { " (gemerkt)" } else { "" }
        ));
    }
    Ok(OpOutput::from(buf))
}

/// Validiert und registriert `path_str` für diese Sitzung, optional mit
/// dauerhaftem Projekt-Merken (`--save`).
///
/// # Errors
/// - [`OpError::NotAvailable`]: keine `ExtraRootsCell` registriert.
/// - [`OpError::InvalidArguments`]: Validierung schlug fehl (siehe
///   [`harw_sandbox::ExtraRootError`]), oder `path_str` konnte für `--save`
///   nicht kanonisiert werden.
/// - [`OpError::Execution`]: Projekt-Config-Pfad, -Öffnen, -Schreiben oder
///   -Speichern schlug bei `--save` fehl.
fn add_one_workdir(ctx: &OpContext, path_str: &str, save: bool) -> Result<OpOutput, OpError> {
    let Some(cell) = ctx.service::<ExtraRootsCell>() else {
        return Err(OpError::NotAvailable(NO_EXTRA_ROOTS_CELL.to_owned()));
    };
    let primary_root = ctx.sandbox().workspace().canonical_root();
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let candidate = PathBuf::from(path_str);

    let added = cell
        .add(&candidate, save, primary_root, user_home.as_deref())
        .map_err(|error| OpError::InvalidArguments(format_extra_root_error(&error)))?;

    let mut note = String::new();
    if save {
        let canonical = candidate.canonicalize().map_err(|error| {
            OpError::InvalidArguments(format!(
                "/add-workdir: '{}' konnte für --save nicht kanonisiert werden: {error}",
                candidate.display()
            ))
        })?;
        let path = crate::permissions::scope_path(ctx, SettingScope::Project)?;
        let mut writer = ConfigWriter::open(&path).map_err(|error| {
            OpError::Execution(format!("Config öffnen fehlgeschlagen: {error}"))
        })?;
        let newly_persisted = writer.append_extra_root(&canonical).map_err(|error| {
            OpError::Execution(format!("Config schreiben fehlgeschlagen: {error}"))
        })?;
        writer.save().map_err(|error| {
            OpError::Execution(format!("Config speichern fehlgeschlagen: {error}"))
        })?;
        note = format!(
            " Dauerhaft in {} gemerkt{}.",
            path.display(),
            if newly_persisted {
                ""
            } else {
                " (war bereits vorhanden)"
            }
        );
    }

    let verb = if added {
        "hinzugefügt"
    } else {
        "bereits registriert"
    };
    Ok(OpOutput::from(format!(
        "Arbeitsverzeichnis {} {}.{note}",
        candidate.display(),
        verb
    )))
}

/// Formatiert einen [`ExtraRootError`] für die Fehlermeldung des Aufrufers.
///
/// # Beschreibung
/// [`ExtraRootError::UserHome`] wird in eine deutsche Meldung übersetzt, die
/// den abgelehnten Pfad ausdrücklich als Home-Verzeichnis benennt — die
/// eigene `Display`-Implementierung von `harw-sandbox` liefert dafür nur
/// englischen Text. Alle anderen Varianten laufen unverändert per `Display`
/// durch (siehe Moduldoku „Fehlermeldungen").
///
/// # Arguments
/// - `error` (`&ExtraRootError`): der von [`ExtraRootsCell::add`]
///   zurückgegebene Validierungsfehler.
///
/// # Returns
/// Der `/add-workdir: …`-präfigierte Meldungstext für
/// [`OpError::InvalidArguments`].
fn format_extra_root_error(error: &ExtraRootError) -> String {
    match error {
        ExtraRootError::UserHome { path } => format!(
            "/add-workdir: '{}' ist das Home-Verzeichnis des Nutzers und kann nicht als \
             zusätzliches Arbeitsverzeichnis registriert werden.",
            path.display()
        ),
        other => format!("/add-workdir: {other}"),
    }
}

/// Entfernt `path_str` aus der Sitzungs-Zelle und, bestes Bemühen, aus der
/// Projekt-Datei.
///
/// # Errors
/// - [`OpError::NotAvailable`]: keine `ExtraRootsCell` registriert.
/// - [`OpError::InvalidArguments`]: `path_str` war nicht registriert.
fn remove_workdir(ctx: &OpContext, path_str: &str) -> Result<OpOutput, OpError> {
    let Some(cell) = ctx.service::<ExtraRootsCell>() else {
        return Err(OpError::NotAvailable(NO_EXTRA_ROOTS_CELL.to_owned()));
    };
    let candidate = PathBuf::from(path_str);
    let canonical = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());

    if !cell.remove(&canonical) {
        return Err(OpError::InvalidArguments(format!(
            "/add-workdir --remove: '{}' war nicht registriert",
            candidate.display()
        )));
    }

    let mut note = String::new();
    if let Ok(path) = crate::permissions::scope_path(ctx, SettingScope::Project) {
        if let Ok(mut writer) = ConfigWriter::open(&path) {
            // Bestes Bemühen: ein interner Schreibfehler (siehe
            // `ConfigError::WriterShapeMismatch`) zählt hier wie "nicht
            // entfernt", genau wie ein fehlgeschlagenes `save()`.
            if writer.remove_extra_root(&canonical).unwrap_or(false) && writer.save().is_ok() {
                note = format!(" Auch dauerhaft aus {} entfernt.", path.display());
            }
        }
    }

    Ok(OpOutput::from(format!(
        "Arbeitsverzeichnis {} entfernt.{note}",
        canonical.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::{AddWorkdirArgs, add_workdir};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::ExtraRootsCell;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen [`OpContext`] dessen Sandbox-Root ein frisches temporäres
    /// Verzeichnis ist, optional mit registrierter [`ExtraRootsCell`].
    fn test_context(with_cell: bool) -> TestResult<(OpContext, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-add-workdir-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let mut services = ServiceMap::new();
        if with_cell {
            services.insert(ExtraRootsCell::new());
        }
        let ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy([Permission::WriteWorkspace, Permission::ReadWorkspace]),
            ),
            services,
        );
        Ok((ctx, root))
    }

    #[test]
    fn test_add_workdir_args_from_raw_args_captures_all_tokens() -> TestResult {
        let args =
            AddWorkdirArgs::from_raw_args(&toks(&["/tmp/x", "--save"])).map_err(ctx("parse"))?;
        assert_eq!(args.tokens, vec!["/tmp/x".to_owned(), "--save".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_add_workdir_args_from_raw_args_empty_is_empty() -> TestResult {
        let args = AddWorkdirArgs::from_raw_args(&toks(&[])).map_err(ctx("parse"))?;
        assert!(args.tokens.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_without_a_cell_is_not_available() -> TestResult {
        let (ctx, root) = test_context(false)?;
        let result = add_workdir(&ctx, AddWorkdirArgs { tokens: Vec::new() }).await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("ExtraRootsCell")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_without_args_lists_empty_state() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let result = add_workdir(&ctx, AddWorkdirArgs { tokens: Vec::new() })
            .await
            .map_err(crate::test_support::ctx("list"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(result.text.contains("Keine zusätzlichen"));
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_registers_a_valid_directory_for_the_session() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let extra = root.join("extra");
        std::fs::create_dir_all(&extra).map_err(crate::test_support::ctx("create extra dir"))?;

        let output = add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec![extra.to_string_lossy().into_owned()],
            },
        )
        .await
        .map_err(crate::test_support::ctx("add workdir"))?;
        assert!(output.text.contains("hinzugefügt"));

        let listed = add_workdir(&ctx, AddWorkdirArgs { tokens: Vec::new() })
            .await
            .map_err(crate::test_support::ctx("list after add"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(listed.text.contains("1 zusätzliche"));
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_rejects_root_directory() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let result = add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec!["/".to_owned()],
            },
        )
        .await;
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(
                    message.contains('/'),
                    "message should mention '/': {message}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_rejects_user_home() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let Some(home) = home else {
            // Kein $HOME in dieser Umgebung gesetzt — Test übersprungen statt fälschlich zu bestehen.
            std::fs::remove_dir_all(&root).ok();
            return Ok(());
        };
        let result = add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec![home.to_string_lossy().into_owned()],
            },
        )
        .await;
        // Erst NACH dem Aufruf aufräumen: `validate_extra_root`
        // (`harw-sandbox/src/extra_roots.rs`) kanonisiert `primary_root` (den
        // hier von `root` abgeleiteten Workspace-Root) noch VOR dem
        // Home-Vergleich. Ein vorzeitiges `remove_dir_all(&root)` ließ diese
        // Kanonisierung mit einem `ExtraRootError::Io` scheitern, bevor der
        // `UserHome`-Zweig je erreicht wurde — die Meldung enthielt dann nie
        // "Home-Verzeichnis", unabhängig von `format_extra_root_error`.
        std::fs::remove_dir_all(&root).ok();
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("Home-Verzeichnis"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_remove_without_path_is_invalid() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let result = add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec!["--remove".to_owned()],
            },
        )
        .await;
        std::fs::remove_dir_all(&root).ok();
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_remove_round_trips_a_registered_directory() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let extra = root.join("extra");
        std::fs::create_dir_all(&extra).map_err(crate::test_support::ctx("create extra dir"))?;
        let extra_str = extra.to_string_lossy().into_owned();

        add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec![extra_str.clone()],
            },
        )
        .await
        .map_err(crate::test_support::ctx("add"))?;

        let removed = add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec!["--remove".to_owned(), extra_str],
            },
        )
        .await
        .map_err(crate::test_support::ctx("remove"))?;
        assert!(removed.text.contains("entfernt"));

        let listed = add_workdir(&ctx, AddWorkdirArgs { tokens: Vec::new() })
            .await
            .map_err(crate::test_support::ctx("list after remove"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(listed.text.contains("Keine zusätzlichen"));
        Ok(())
    }

    #[tokio::test]
    async fn add_workdir_remove_of_unregistered_path_is_invalid() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let extra = root.join("never-added");
        std::fs::create_dir_all(&extra).map_err(crate::test_support::ctx("create dir"))?;
        let result = add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec!["--remove".to_owned(), extra.to_string_lossy().into_owned()],
            },
        )
        .await;
        std::fs::remove_dir_all(&root).ok();
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }
}
