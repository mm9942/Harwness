//! `fs.edit` — Tool-Executor für gezielte Textersetzung in einer Datei.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.edit`-Executor:
//! - [`FsEditExecutor`]: implementiert [`ToolExecutor`]; ersetzt in einer
//!   vorhandenen UTF-8-Datei des Workspace `old_string` durch `new_string`.
//!   Ohne `replace_all` muss `old_string` genau einmal vorkommen.
//!
//! # Sicherheitsregeln (wie `fs.write`, W1-02)
//! - Recht `WriteWorkspace` (fail closed); nie automatisch freigegeben.
//! - Pfad lexikalisch geprüft (kein `..`, nicht absolut), geschützte Bereiche
//!   `.git/` und `.harw/` abgelehnt.
//! - Elternverzeichnis **und** Zieldatei werden symlinkfrei unterhalb der
//!   Workspace-Wurzel geöffnet; ein Symlink am Ziel wird nicht verfolgt,
//!   sondern abgelehnt (die Datei muss eine vorhandene reguläre Datei sein).
//! - Größenobergrenze [`MAX_EDIT_FILE_BYTES`] für Ausgangs- und Ergebnisdatei.
//! - Geschrieben wird atomar über [`harw_fsutil::write_atomic`] in das
//!   geöffnete Elternverzeichnis (`/proc/self/fd/<fd>`); die Rechte-Bits der
//!   vorhandenen Datei bleiben erhalten.
//!
//! # Ergebnis
//! `ToolOutput::Json { path, replacements, diff_excerpt }`; `diff_excerpt`
//! enthält je Ersetzung die betroffenen Zeilen mit `-`/`+` und ist auf
//! [`MAX_DIFF_EXCERPT_BYTES`] gekürzt.
//!
//! # Schlüsseltypen
//! - [`FsEditExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsEditExecutor`] ist `Send + Sync`. `fs.edit` ist NICHT `parallel_safe`.
//! Die Datei-IO läuft im Blocking-Pool.
//!
//! # Fehler
//! Permission-, Pfad-, Treffer- und I/O-Fehler → `Ok(ToolOutput::error(...))`.
//! Kein Panic.

use crate::error::FsToolError;
use crate::symlink::resolve_for_write;
use crate::tree::{MAX_SCAN_FILE_BYTES, Workspace, normalize_relative, open_file_in, read_bounded};
use crate::write::protected_component;
use harw_authority::Permission;
use harw_fsutil::{AtomicWriteOptions, write_atomic};
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::Deserialize;
use std::os::unix::fs::PermissionsExt;

/// Größenobergrenze (Bytes) für die zu bearbeitende Datei und ihr Ergebnis.
///
/// Dieselbe Obergrenze, die die lesenden fs-Werkzeuge für ganze Dateien
/// anwenden (`MAX_SCAN_FILE_BYTES`, 8 MiB).
pub const MAX_EDIT_FILE_BYTES: u64 = MAX_SCAN_FILE_BYTES;

/// Höchstlänge (Bytes) des Diff-Ausschnitts im Ergebnis.
pub const MAX_DIFF_EXCERPT_BYTES: usize = 2048;

/// Markierung am Ende eines gekürzten Diff-Ausschnitts.
const DIFF_TRUNCATION_MARKER: &str = "… [Diff gekürzt]";

/// Werkzeugname für Meldungen und Rechteprüfung.
const TOOL: &str = "fs.edit";

/// Deserialisierte Argumente für `fs.edit`.
#[derive(Debug, Deserialize)]
struct FsEditArgs {
    /// Pfad relativ zum Workspace-Root.
    path: String,
    /// Zu ersetzender Text (nicht leer).
    old_string: String,
    /// Ersatztext (muss sich von `old_string` unterscheiden).
    new_string: String,
    /// Alle Vorkommen ersetzen statt genau eines.
    #[serde(default)]
    replace_all: bool,
}

/// Führt `fs.edit`-Aufrufe aus.
///
/// # Description
/// Ersetzt in einer vorhandenen UTF-8-Datei relativ zum Workspace-Root
/// `old_string` durch `new_string` und schreibt das Ergebnis atomar zurück.
/// Ohne `replace_all` muss `old_string` genau einmal vorkommen; sonst meldet
/// das Werkzeug die Trefferzahl als Fehler.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. NICHT `parallel_safe`.
///
/// # Errors
/// Permission-, Pfad-, Treffer- und I/O-Fehler → `Ok(ToolOutput::error(...))`.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsEditExecutor;
/// let _executor = FsEditExecutor;
/// ```
pub struct FsEditExecutor;

impl FsEditExecutor {
    /// Führt die Ersetzung synchron (blockierend) aus.
    ///
    /// # Errors
    /// Gibt `Err(ToolsError::InvalidArguments)` bei ungültigen JSON-Argumenten.
    /// Alle anderen Fehler → `Ok(ToolOutput::error(...))`.
    fn edit_file(
        &self,
        ctx: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        if let Some(err) =
            harw_tools::sandbox_guard::require_permission(ctx, Permission::WriteWorkspace, TOOL)
        {
            return Ok(err);
        }

        let args: FsEditArgs = match serde_json::from_value::<FsEditArgs>(call.arguments.clone()) {
            Ok(a) => a,
            Err(err) => {
                return Err(ToolsError::InvalidArguments {
                    name: TOOL.to_owned(),
                    reason: err.to_string(),
                });
            }
        };
        if args.old_string.is_empty() {
            return Ok(ToolOutput::error(
                "fs.edit: 'old_string' darf nicht leer sein (neue Dateien mit fs.write anlegen)",
            ));
        }
        if args.old_string == args.new_string {
            return Ok(ToolOutput::error(
                "fs.edit: 'old_string' und 'new_string' sind identisch — nichts zu ändern",
            ));
        }

        let relative = match normalize_relative(&args.path) {
            Ok(rel) => rel,
            Err(reason) => return Ok(ToolOutput::error(format!("fs.edit: {reason}"))),
        };
        // Symlinks im Pfad nur folgen, wenn das Ziel im Workspace bleibt
        // (siehe `crate::symlink`); der Schutzbereich gilt für das Ziel.
        let relative =
            match resolve_for_write(ctx.sandbox().workspace().canonical_root(), &relative) {
                Ok(resolved) => resolved,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => relative,
                Err(err) => {
                    return Ok(ToolOutput::error(format!(
                        "fs.edit: '{}': {err}",
                        args.path
                    )));
                }
            };
        if let Some(protected) = protected_component(&relative) {
            return Ok(ToolOutput::error(format!(
                "fs.edit: '{}' liegt im geschützten Bereich '{protected}/' und wird nicht \
                 bearbeitet",
                args.path
            )));
        }
        let (Some(name), Some(parent_rel)) = (relative.file_name(), relative.parent()) else {
            return Ok(ToolOutput::error(format!(
                "fs.edit: '{}' enthält keinen Dateinamen",
                args.path
            )));
        };

        // Elternverzeichnis und Zieldatei symlinkfrei öffnen.
        let workspace = match Workspace::open(ctx.sandbox().workspace().canonical_root()) {
            Ok(workspace) => workspace,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        let parent = match workspace.open_dir(parent_rel) {
            Ok(parent) => parent,
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.edit: Elternverzeichnis von '{}' ist nicht nutzbar: {err}",
                    args.path
                )));
            }
        };
        let file = match open_file_in(&parent, name) {
            Ok(file) => file,
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.edit: '{}' kann nicht geöffnet werden (die Datei muss existieren und \
                     eine reguläre Datei sein): {err}",
                    args.path
                )));
            }
        };
        let mode = match file.metadata() {
            Ok(meta) => meta.permissions().mode() & 0o777,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        let bytes = match read_bounded(&file, MAX_EDIT_FILE_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                return Ok(ToolOutput::error(format!(
                    "fs.edit: '{}' ist größer als {MAX_EDIT_FILE_BYTES} Bytes und wird nicht \
                     bearbeitet",
                    args.path
                )));
            }
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        drop(file);
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                return Ok(ToolOutput::error(format!(
                    "fs.edit: '{}' ist keine gültige UTF-8-Textdatei",
                    args.path
                )));
            }
        };

        let positions: Vec<usize> = content
            .match_indices(args.old_string.as_str())
            .map(|(index, _)| index)
            .collect();
        let count = positions.len();
        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "fs.edit: 'old_string' kommt in '{}' nicht vor (0 Treffer)",
                args.path
            )));
        }
        if count > 1 && !args.replace_all {
            return Ok(ToolOutput::error(format!(
                "fs.edit: 'old_string' kommt in '{}' {count}-mal vor; mehr Kontext angeben, \
                 damit er eindeutig ist, oder 'replace_all': true setzen",
                args.path
            )));
        }

        let updated = if args.replace_all {
            content.replace(args.old_string.as_str(), &args.new_string)
        } else {
            content.replacen(args.old_string.as_str(), &args.new_string, 1)
        };
        if u64::try_from(updated.len()).unwrap_or(u64::MAX) > MAX_EDIT_FILE_BYTES {
            return Ok(ToolOutput::error(format!(
                "fs.edit: Ergebnis wäre größer als {MAX_EDIT_FILE_BYTES} Bytes; nicht geschrieben"
            )));
        }

        // Solange `parent` offen ist, bezeichnet dieser Pfad genau das geöffnete
        // Verzeichnis (`/proc/self/fd/<fd>`).
        let target = workspace.dir_path(&parent, parent_rel).join(name);
        if let Err(err) = write_atomic(
            &target,
            updated.as_bytes(),
            AtomicWriteOptions::with_mode(mode),
        ) {
            return Ok(ToolOutput::error(FsToolError::Io(err).to_string()));
        }

        let diff_excerpt = diff_excerpt(&content, &positions, &args.old_string, &args.new_string);
        Ok(ToolOutput::json(serde_json::json!({
            "path": args.path,
            "replacements": count,
            "diff_excerpt": diff_excerpt,
        })))
    }
}

/// Baut den gekürzten Diff-Ausschnitt: je Treffer eine Kopfzeile
/// `@@ Zeile N @@`, die betroffenen Originalzeilen mit `-` und die neuen
/// Zeilen mit `+`. Höchstens [`MAX_DIFF_EXCERPT_BYTES`] Bytes (UTF-8-sicher
/// gekürzt, mit Markierung).
fn diff_excerpt(content: &str, positions: &[usize], old: &str, new: &str) -> String {
    let mut excerpt = String::new();
    // Zeilennummern inkrementell zählen (nicht je Treffer ab Dateianfang).
    let mut counted_up_to = 0usize;
    let mut newlines_before = 0usize;
    for &start in positions {
        let end = start.saturating_add(old.len());
        let line_start = content
            .get(..start)
            .and_then(|head| head.rfind('\n'))
            .map_or(0, |index| index.saturating_add(1));
        let line_end = content
            .get(end..)
            .and_then(|tail| tail.find('\n'))
            .map_or(content.len(), |index| end.saturating_add(index));
        let prefix = content.get(line_start..start).unwrap_or("");
        let suffix = content.get(end..line_end).unwrap_or("");
        newlines_before = newlines_before.saturating_add(
            content
                .get(counted_up_to..start)
                .map_or(0, |span| span.matches('\n').count()),
        );
        counted_up_to = start;
        let line_no = newlines_before.saturating_add(1);

        let mut hunk = format!("@@ Zeile {line_no} @@\n");
        for line in format!("{prefix}{old}{suffix}").split('\n') {
            hunk.push('-');
            hunk.push_str(line);
            hunk.push('\n');
        }
        for line in format!("{prefix}{new}{suffix}").split('\n') {
            hunk.push('+');
            hunk.push_str(line);
            hunk.push('\n');
        }

        if excerpt.len().saturating_add(hunk.len()) > MAX_DIFF_EXCERPT_BYTES {
            let room = MAX_DIFF_EXCERPT_BYTES.saturating_sub(excerpt.len());
            let mut cut = room.min(hunk.len());
            while cut > 0 && !hunk.is_char_boundary(cut) {
                cut -= 1;
            }
            excerpt.push_str(hunk.get(..cut).unwrap_or(""));
            if !excerpt.ends_with('\n') && !excerpt.is_empty() {
                excerpt.push('\n');
            }
            excerpt.push_str(DIFF_TRUNCATION_MARKER);
            return excerpt;
        }
        excerpt.push_str(&hunk);
    }
    if excerpt.ends_with('\n') {
        excerpt.pop();
    }
    excerpt
}

impl ToolExecutor for FsEditExecutor {
    /// Führt eine `fs.edit`-Invokation aus.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-etablierte Autorität mit
    ///   Sandbox-Spec (Permissions + Workspace).
    /// - `call` (`&ToolCall`): die ungeprüfte Invokation mit JSON-Argumenten.
    ///
    /// # Returns
    /// `Ok(ToolOutput::Json { path, replacements, diff_excerpt })` bei Erfolg,
    /// `Ok(ToolOutput::Error { message })` bei Permission-, Treffer- oder
    /// I/O-Fehler.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: fehlende oder fehlerhafte JSON-Argumente.
    ///
    /// # Concurrency
    /// Nicht parallel-safe; läuft im Blocking-Pool.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let context = context.clone();
        let call = call.clone();
        Box::pin(crate::blocking::run_blocking(TOOL, move || {
            FsEditExecutor.edit_file(&context, &call)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, SECRET, TestError, TestResult, call, render};
    use std::fs;

    /// Führt `fs.edit` mit `args` in einem Workspace mit `WriteWorkspace` aus.
    fn run(fixture: &Fixture, args: serde_json::Value) -> TestResult<ToolOutput> {
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;
        Ok(FsEditExecutor.edit_file(&ctx, &call("fs.edit", args))?)
    }

    /// Erwartet eine Fehlerausgabe und liefert ihre Meldung.
    fn expect_error(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Error { message } => Ok(message),
            other => Err(TestError::Unexpected(format!(
                "Fehler erwartet, erhalten: {other:?}"
            ))),
        }
    }

    /// Erwartet eine JSON-Ausgabe.
    fn expect_json(output: ToolOutput) -> TestResult<serde_json::Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!(
                "JSON erwartet, erhalten: {other:?}"
            ))),
        }
    }

    #[test]
    fn eindeutiger_treffer_wird_ersetzt() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new()?;
        let target = fixture.ws.join("main.rs");
        fs::write(&target, "fn main() {\n    let x = 1;\n}\n")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;

        let value = expect_json(run(
            &fixture,
            serde_json::json!({
                "path": "main.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;"
            }),
        )?)?;
        assert_eq!(
            fs::read_to_string(&target)?,
            "fn main() {\n    let x = 2;\n}\n"
        );
        assert_eq!(value["path"], "main.rs");
        assert_eq!(value["replacements"], 1);
        let diff = value["diff_excerpt"]
            .as_str()
            .ok_or(TestError::Missing("diff_excerpt"))?;
        assert!(diff.contains("@@ Zeile 2 @@"), "{diff}");
        assert!(diff.contains("-    let x = 1;"), "{diff}");
        assert!(diff.contains("+    let x = 2;"), "{diff}");
        assert_eq!(
            fs::metadata(&target)?.permissions().mode() & 0o777,
            0o600,
            "Rechte-Bits bleiben erhalten"
        );
        let names = fs::read_dir(&fixture.ws)?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<String>>>()?;
        assert_eq!(names, vec!["main.rs".to_owned()], "keine Tempdatei-Reste");
        Ok(())
    }

    #[test]
    fn mehrdeutiger_treffer_ist_fehler_mit_trefferzahl() -> TestResult {
        let fixture = Fixture::new()?;
        let target = fixture.ws.join("a.txt");
        fs::write(&target, "foo\nfoo\nfoo\n")?;

        let message = expect_error(run(
            &fixture,
            serde_json::json!({ "path": "a.txt", "old_string": "foo", "new_string": "bar" }),
        )?)?;
        assert!(message.contains("3-mal"), "{message}");
        assert!(message.contains("replace_all"), "{message}");
        assert_eq!(
            fs::read_to_string(&target)?,
            "foo\nfoo\nfoo\n",
            "unverändert"
        );
        Ok(())
    }

    #[test]
    fn replace_all_ersetzt_alle_treffer() -> TestResult {
        let fixture = Fixture::new()?;
        let target = fixture.ws.join("a.txt");
        fs::write(&target, "foo\nx foo y\nfoo\n")?;

        let value = expect_json(run(
            &fixture,
            serde_json::json!({
                "path": "a.txt",
                "old_string": "foo",
                "new_string": "bar",
                "replace_all": true
            }),
        )?)?;
        assert_eq!(fs::read_to_string(&target)?, "bar\nx bar y\nbar\n");
        assert_eq!(value["replacements"], 3);
        let diff = value["diff_excerpt"]
            .as_str()
            .ok_or(TestError::Missing("diff_excerpt"))?;
        assert!(diff.contains("-x foo y\n+x bar y"), "{diff}");
        Ok(())
    }

    #[test]
    fn diff_ausschnitt_wird_gekuerzt() -> TestResult {
        let fixture = Fixture::new()?;
        let target = fixture.ws.join("big.txt");
        fs::write(&target, "abc\n".repeat(2000))?;

        let value = expect_json(run(
            &fixture,
            serde_json::json!({
                "path": "big.txt",
                "old_string": "abc",
                "new_string": "äöü",
                "replace_all": true
            }),
        )?)?;
        assert_eq!(value["replacements"], 2000);
        let diff = value["diff_excerpt"]
            .as_str()
            .ok_or(TestError::Missing("diff_excerpt"))?;
        assert!(diff.ends_with(DIFF_TRUNCATION_MARKER), "{diff}");
        assert!(diff.len() <= MAX_DIFF_EXCERPT_BYTES + DIFF_TRUNCATION_MARKER.len() + 1);
        Ok(())
    }

    #[test]
    fn nicht_gefunden_ist_fehler() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("a.txt"), "hallo\n")?;

        let message = expect_error(run(
            &fixture,
            serde_json::json!({ "path": "a.txt", "old_string": "tschüss", "new_string": "x" }),
        )?)?;
        assert!(message.contains("0 Treffer"), "{message}");

        // Fehlende Datei: fs.edit legt nichts an.
        let message = expect_error(run(
            &fixture,
            serde_json::json!({ "path": "neu.txt", "old_string": "a", "new_string": "b" }),
        )?)?;
        assert!(message.contains("existieren"), "{message}");
        assert!(!fixture.ws.join("neu.txt").exists());
        Ok(())
    }

    #[test]
    fn leeres_oder_identisches_old_string_wird_abgelehnt() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("a.txt"), "hallo\n")?;

        let message = expect_error(run(
            &fixture,
            serde_json::json!({ "path": "a.txt", "old_string": "", "new_string": "x" }),
        )?)?;
        assert!(message.contains("nicht leer"), "{message}");

        let message = expect_error(run(
            &fixture,
            serde_json::json!({ "path": "a.txt", "old_string": "hallo", "new_string": "hallo" }),
        )?)?;
        assert!(message.contains("identisch"), "{message}");
        assert_eq!(fs::read_to_string(fixture.ws.join("a.txt"))?, "hallo\n");
        Ok(())
    }

    #[test]
    fn symlink_ausbruch_wird_abgelehnt() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        let outside = fixture
            .outside
            .to_str()
            .ok_or(TestError::Missing("outside as str"))?
            .to_owned();

        for path in [
            "link_file",
            "link_dir/secret.txt",
            "loop/link_file",
            "nested/up/link_file",
        ] {
            let output = run(
                &fixture,
                serde_json::json!({ "path": path, "old_string": SECRET, "new_string": "x" }),
            )?;
            let rendered = render(&output)?;
            assert!(
                rendered.contains("Symlink zeigt außerhalb des Arbeitsbereichs")
                    && rendered.contains(&outside),
                "{path}: die Ablehnung nennt das Ziel: {rendered}"
            );
            expect_error(output)?;
        }
        assert_eq!(
            fs::read_to_string(fixture.outside.join("secret.txt"))?,
            SECRET
        );
        assert!(
            fs::symlink_metadata(fixture.ws.join("link_file"))?
                .file_type()
                .is_symlink(),
            "Symlink bleibt unangetastet"
        );
        Ok(())
    }

    #[test]
    fn pfade_ausserhalb_des_workspace_werden_abgelehnt() -> TestResult {
        let fixture = Fixture::new()?;
        let secret = fixture.outside.join("secret.txt");
        let absolute = secret
            .to_str()
            .ok_or(TestError::Missing("secret as str"))?
            .to_owned();

        for path in ["../outside/secret.txt", absolute.as_str(), ".git/config"] {
            let message = expect_error(run(
                &fixture,
                serde_json::json!({ "path": path, "old_string": SECRET, "new_string": "x" }),
            )?)?;
            assert!(message.starts_with("fs.edit:"), "{path}: {message}");
        }
        assert_eq!(fs::read_to_string(&secret)?, SECRET);
        Ok(())
    }

    #[test]
    fn nicht_utf8_wird_abgelehnt() -> TestResult {
        let fixture = Fixture::new()?;
        let target = fixture.ws.join("bin.dat");
        fs::write(&target, [0x66, 0x6f, 0x6f, 0xff, 0xfe])?;

        let message = expect_error(run(
            &fixture,
            serde_json::json!({ "path": "bin.dat", "old_string": "foo", "new_string": "bar" }),
        )?)?;
        assert!(message.contains("UTF-8"), "{message}");
        assert_eq!(fs::read(&target)?, vec![0x66, 0x6f, 0x6f, 0xff, 0xfe]);
        Ok(())
    }

    #[test]
    fn ohne_schreibrecht_wird_abgelehnt() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("a.txt"), "foo\n")?;
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let output = FsEditExecutor.edit_file(
            &ctx,
            &call(
                "fs.edit",
                serde_json::json!({ "path": "a.txt", "old_string": "foo", "new_string": "bar" }),
            ),
        )?;
        let message = expect_error(output)?;
        assert!(message.contains("WriteWorkspace"), "{message}");
        assert_eq!(fs::read_to_string(fixture.ws.join("a.txt"))?, "foo\n");
        Ok(())
    }

    #[test]
    fn ungueltige_argumente_sind_invalid_arguments() -> TestResult {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;
        let result = FsEditExecutor.edit_file(
            &ctx,
            &call("fs.edit", serde_json::json!({ "path": "a.txt" })),
        );
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "{result:?}"
        );
        Ok(())
    }
}
