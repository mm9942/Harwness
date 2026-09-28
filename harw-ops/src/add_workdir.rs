//! `/add-workdir` — zusätzliche Arbeitsverzeichnisse für die Sitzung freigeben.
//!
//! Spec-Quelle: Contract `docs/design/config-scopes.md` §2 (Slice A8,
//! `harw_sandbox::ExtraRootsCell`).
//!
//! # Verantwortung
//! Diese Operation validiert einen Verzeichnis-Kandidaten über
//! [`harw_sandbox::validate_extra_root`] und registriert ihn danach über
//! [`ExtraRootsCell::add`] (das dieselbe Validierung intern erneut
//! durchläuft) in der geteilten [`ExtraRootsCell`] der Sitzung. Sie erweitert
//! **nie** implizit eine Sandbox: die Zelle wird nur dann wirksam, wenn eine
//! `SandboxSpec` sie ausdrücklich über `with_extra_roots` gebunden hat — das
//! ist Sache der Kompositionswurzel (`harw-runtime`), nicht dieser Operation.
//! Bei `--save` läuft die Validierung zusätzlich **vor** jedem
//! Persistenz-Versuch, und die Persistenz selbst läuft **vor** der
//! Registrierung in der Zelle: schlägt eine der beiden fehl, bleibt die
//! Zelle unverändert, statt eine Wurzel zu zeigen, die nie geschrieben wurde
//! (siehe `add_one_workdir`).
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
use harw_sandbox::{ExtraRootError, ExtraRootsCell, validate_extra_root};

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
/// # Beschreibung
/// Bei `--save` läuft die Reihenfolge bewusst **validieren, dann
/// persistieren, dann in die Zelle aufnehmen** — nicht umgekehrt. Vorher
/// registrierte [`ExtraRootsCell::add`] die Wurzel bereits mit
/// `persisted = true` in der Zelle, bevor überhaupt versucht wurde, sie zu
/// schreiben; schlug das Schreiben danach fehl, blieb die Wurzel trotzdem in
/// der Zelle und `/permissions` listete sie als „(gemerkt)“, obwohl in der
/// Config-Datei nichts stand. Jetzt bricht ein ungültiger Kandidat oder ein
/// Persistenzfehler ab, bevor [`ExtraRootsCell::add`] je aufgerufen wird —
/// die Zelle bleibt in beiden Fällen unverändert.
///
/// Eine Ausnahme bleibt das Sitzungslimit ([`ExtraRootError::TooMany`]): das
/// prüft ausschließlich [`ExtraRootsCell::add`] selbst, weil nur die Zelle
/// ihren aktuellen Füllstand kennt. Scheitert ein sonst gültiger Kandidat
/// erst daran, wurde er für `--save` bereits geschrieben, obwohl die
/// Sitzungs-Zelle ihn ablehnt. Das ist kein neues Risiko: eine künftige
/// Sitzung überspringt einen solchen Eintrag beim Neuladen genauso wie jeden
/// anderen veralteten (`harw-runtime::assembly::seed_extra_roots` protokolliert
/// das nur als Warnung, ohne die Montage zu gefährden) — anders als bei einer
/// tatsächlich ungültigen Wurzel (Home, `/`, Vorfahre) wird so nie eine
/// Wurzel in der laufenden Sitzung wirksam, die nicht auch validiert wurde.
///
/// # Errors
/// - [`OpError::NotAvailable`]: keine `ExtraRootsCell` registriert.
/// - [`OpError::InvalidArguments`]: Validierung schlug fehl (siehe
///   [`harw_sandbox::ExtraRootError`]).
/// - [`OpError::Execution`]: Projekt-Config-Pfad, -Öffnen, -Schreiben oder
///   -Speichern schlug bei `--save` fehl.
fn add_one_workdir(ctx: &OpContext, path_str: &str, save: bool) -> Result<OpOutput, OpError> {
    let Some(cell) = ctx.service::<ExtraRootsCell>() else {
        return Err(OpError::NotAvailable(NO_EXTRA_ROOTS_CELL.to_owned()));
    };
    let primary_root = ctx.sandbox().workspace().canonical_root();
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let candidate = PathBuf::from(path_str);

    let mut note = String::new();
    if save {
        // Validieren und persistieren, BEVOR die Zelle überhaupt berührt
        // wird (siehe Funktionsdoku oben) — bei einem Fehler hier ist
        // `cell.add` unten noch nicht gelaufen, die Zelle bleibt also exakt
        // im Zustand von vor diesem Aufruf.
        let canonical = validate_extra_root(&candidate, primary_root, user_home.as_deref())
            .map_err(|error| OpError::InvalidArguments(format_extra_root_error(&error)))?;
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

    // Erst jetzt, nach erfolgreicher Persistenz (falls `--save`), wird die
    // Zelle erweitert bzw. das `persisted`-Flag einer bereits registrierten
    // Wurzel gesetzt.
    let added = cell
        .add(&candidate, save, primary_root, user_home.as_deref())
        .map_err(|error| OpError::InvalidArguments(format_extra_root_error(&error)))?;

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
/// - `error` (`&ExtraRootError`): der von [`harw_sandbox::validate_extra_root`]
///   oder [`ExtraRootsCell::add`] zurückgegebene Validierungsfehler — beide
///   verwenden intern dieselbe Prüfung und liefern denselben Fehlertyp.
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
    use harw_home::{ProjectKind, ProjectRoot, ResolvedHomeContext};
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::ExtraRootsCell;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::Arc;
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

    /// Baut einen [`ResolvedHomeContext`] für `home` mit einem simplen
    /// Fake-Projekt (`ProjectKind::Directory` — kein `.git` nötig, die
    /// Tests unten prüfen nur die Persistenz unter `<home>`, nicht die
    /// Projekt-Erkennung selbst). Analog zu
    /// `crate::permissions::tests::resolved_home_context`.
    fn resolved_home_context(home: &std::path::Path) -> TestResult<Arc<ResolvedHomeContext>> {
        let project_dir = home.join("project");
        std::fs::create_dir_all(&project_dir).map_err(ctx("create project dir"))?;
        let project = ProjectRoot {
            root: project_dir.clone(),
            trust_key: project_dir,
            kind: ProjectKind::Directory,
        };
        Ok(Arc::new(
            ResolvedHomeContext::new(home, "default".to_owned(), project)
                .map_err(ctx("build resolved home context"))?,
        ))
    }

    /// Wie [`test_context`] (immer mit einer eigenen [`ExtraRootsCell`]),
    /// zusätzlich mit einem echten [`ResolvedHomeContext`] für `home` in der
    /// `ServiceMap` — für Tests, die `--save` gegen einen echten
    /// Persistenz-Pfad fahren, statt gegen `HARW_HOME`/`$HOME` der
    /// Testumgebung (Contract §2, siehe `crate::permissions::scope_path`).
    /// Analog zu `crate::permissions::tests::test_context_with_resolved_home`.
    fn test_context_with_resolved_home(
        home: &std::path::Path,
    ) -> TestResult<(OpContext, PathBuf, ExtraRootsCell)> {
        let (base, root) = test_context(false)?;
        let home_context = resolved_home_context(home)?;
        let cell = ExtraRootsCell::new();
        let mut services = ServiceMap::new();
        services.insert(cell.clone());
        services.insert(home_context);
        let op_ctx = OpContext::new(
            base.session_id().clone(),
            base.turn_id().clone(),
            base.sandbox().clone(),
            services,
        );
        Ok((op_ctx, root, cell))
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

    /// Deckt ab, dass `--save` auf einen bereits von `validate_extra_root`
    /// abgelehnten Kandidaten (`/`) die Zelle nicht füllt — ohne dass dafür
    /// je `crate::permissions::scope_path` (und damit `HARW_HOME`) berührt
    /// werden muss, weil `validate_extra_root` hier schon beim
    /// `RootDirectory`-Check abbricht, bevor der Persistenz-Zweig überhaupt
    /// erreicht wird.
    ///
    /// Dieser Test allein prüft die in der Moduldoku beschriebene
    /// Reihenfolge (validieren → persistieren → `cell.add`) NICHT: bei
    /// einem bereits ungültigen Kandidaten liefert auch der alte Code (der
    /// `cell.add` vor der Persistenz aufrief) denselben Fehler ohne die
    /// Zelle je zu berühren, weil [`ExtraRootsCell::add`] intern selbst
    /// zuerst validiert. Die eigentliche Reihenfolge — ein *gültiger*
    /// Kandidat, dessen Persistenz erst nach erfolgreicher Validierung läuft
    /// und dessen `cell.add` erst nach erfolgreicher Persistenz — decken
    /// `add_workdir_save_of_a_valid_directory_persists_and_registers` und
    /// `add_workdir_save_of_a_valid_directory_leaves_the_cell_untouched_when_persistence_fails`
    /// unten ab.
    #[tokio::test]
    async fn add_workdir_save_of_root_directory_leaves_the_cell_untouched() -> TestResult {
        let (ctx, root) = test_context(true)?;
        let Some(cell) = ctx.service::<ExtraRootsCell>() else {
            std::fs::remove_dir_all(&root).ok();
            return Err(TestError::Unexpected(
                "test_context(true) sollte eine ExtraRootsCell registrieren".to_owned(),
            ));
        };
        let result = add_workdir(
            &ctx,
            AddWorkdirArgs {
                tokens: vec!["/".to_owned(), "--save".to_owned()],
            },
        )
        .await;
        let remaining = cell.snapshot().len();
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
        assert_eq!(
            remaining, 0,
            "--save auf einen ungültigen Kandidaten darf die Zelle nicht füllen"
        );
        Ok(())
    }

    /// Regressionstest für die eigentliche Ordering-Reihenfolge: `--save` auf
    /// einen *gültigen* Kandidaten muss den Persistenz-Zweig
    /// (`ConfigWriter::open`/`append_extra_root`/`save`) tatsächlich
    /// durchlaufen — die Datei unter dem projektbezogenen Pfad des
    /// gebundenen `ResolvedHomeContext` enthält danach den kanonischen
    /// Kandidaten — und danach `ExtraRootsCell::add` erreichen, statt eine
    /// der beiden Wirkungen bloß zu behaupten.
    #[tokio::test]
    async fn add_workdir_save_of_a_valid_directory_persists_and_registers() -> TestResult {
        let home_dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = home_dir.path().join("home");
        let (op_ctx, root, cell) = test_context_with_resolved_home(&home)?;
        let extra = root.join("extra");
        std::fs::create_dir_all(&extra).map_err(ctx("create extra dir"))?;
        let canonical_extra = extra.canonicalize().map_err(ctx("canonicalize extra dir"))?;

        let output = add_workdir(
            &op_ctx,
            AddWorkdirArgs {
                tokens: vec![extra.to_string_lossy().into_owned(), "--save".to_owned()],
            },
        )
        .await
        .map_err(crate::test_support::ctx("add --save"))?;
        assert!(output.text.contains("gemerkt"), "{}", output.text);

        let project_settings_path =
            crate::permissions::scope_path(&op_ctx, harw_config::SettingScope::Project)
                .map_err(crate::test_support::ctx("scope_path"))?;
        let persisted =
            std::fs::read_to_string(&project_settings_path).map_err(ctx("read persisted config"))?;
        std::fs::remove_dir_all(&root).ok();
        assert!(
            persisted.contains(&canonical_extra.to_string_lossy().into_owned()),
            "{persisted}"
        );

        let roots = cell.snapshot();
        assert_eq!(
            roots.len(),
            1,
            "cell.add sollte nach erfolgreicher Persistenz erreicht werden"
        );
        assert!(
            roots[0].persisted,
            "die registrierte Wurzel sollte persisted=true tragen"
        );
        Ok(())
    }

    /// Regressionstest (Ordering-Fix): schlägt die Persistenz bei `--save`
    /// fehl, obwohl der Kandidat `validate_extra_root` bestanden hat, darf
    /// `ExtraRootsCell::add` nie aufgerufen werden — die Zelle bleibt exakt
    /// im Zustand von vor diesem Aufruf. Erzwingt den Fehler wie
    /// `crate::permissions::tests::permissions_allow_global_does_not_activate_the_rule_when_persistence_fails`:
    /// der Projekt-Config-Pfad ist bereits ein Verzeichnis, `ConfigWriter::open`
    /// scheitert beim Lesen.
    #[tokio::test]
    async fn add_workdir_save_of_a_valid_directory_leaves_the_cell_untouched_when_persistence_fails()
    -> TestResult {
        let home_dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = home_dir.path().join("home");
        let (op_ctx, root, cell) = test_context_with_resolved_home(&home)?;
        let extra = root.join("extra");
        std::fs::create_dir_all(&extra).map_err(ctx("create extra dir"))?;

        let project_settings_path =
            crate::permissions::scope_path(&op_ctx, harw_config::SettingScope::Project)
                .map_err(crate::test_support::ctx("scope_path"))?;
        std::fs::create_dir_all(&project_settings_path)
            .map_err(ctx("block config path with a directory"))?;

        let result = add_workdir(
            &op_ctx,
            AddWorkdirArgs {
                tokens: vec![extra.to_string_lossy().into_owned(), "--save".to_owned()],
            },
        )
        .await;
        std::fs::remove_dir_all(&root).ok();

        match result {
            Err(OpError::Execution(message)) => {
                assert!(message.contains("Config"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Execution error, got {other:?}"
                )));
            }
        }
        assert_eq!(
            cell.snapshot().len(),
            0,
            "fehlgeschlagene Persistenz darf die Zelle nicht füllen"
        );
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
