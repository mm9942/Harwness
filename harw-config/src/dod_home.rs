//! `dod_home` — schreibt die DoD-Einstellungen der Home-Ebene in
//! `<harw_home>/config.toml` (PL-100 WP 1.9, Entscheidung O12).
//!
//! # Verantwortung
//! - [`set_dod_triage_model`] setzt oder entfernt `[dod.triage] model`
//!   (Aufrufer: `/dod model set|clear`, TUI-Settings). Die Home-Ebene ist
//!   eine der beiden Ebenen, aus denen `[dod.triage]` überhaupt gilt
//!   (`crate::discovery::discover_config`); ein `model` im aktiven Profil hat
//!   allerdings Vorrang.
//! - [`ensure_dod_defaults`] ergänzt `[dod.approve] tui_tty = true` in einer
//!   vorhandenen `config.toml`, wenn der Schlüssel fehlt (Aufrufer: `harw
//!   update` und `harw agent install-record`, also die echten
//!   Installationspfade). Ein ausdrückliches `tui_tty = false` bleibt immer
//!   stehen. Eine fehlende Datei legt die Funktion nie an (das übernimmt das
//!   Scaffolding von `harw-home` mit seiner Vorlage), und eine `config.toml`,
//!   die ein Symlink oder keine reguläre Datei ist, fasst sie nicht an: der
//!   atomare Schreibpfad würde den Symlink durch eine reguläre Datei
//!   ersetzen.
//!
//! Beide Funktionen editieren das Dokument mit `toml_edit` über
//! [`ConfigWriter`]: Kommentare, Formatierung und alle übrigen Schlüssel
//! bleiben erhalten, auch wenn `[dod]` als Inline- oder Punkt-Tabelle
//! geschrieben ist. Vor dem Schreiben wird die resultierende `[dod]`-Tabelle
//! gegen [`DodSection`] geprüft (fail closed); ein unverändertes Dokument
//! wird nicht geschrieben (keine Backup-Rotation). Geschrieben wird atomar
//! mit Modus `0600` und einem rotierten Backup (siehe [`ConfigWriter::save`]).
//!
//! # Nebenläufigkeit
//! Synchron und blockierend (Datei-I/O). Parallele Schreiber auf dieselbe
//! Datei schützt die Sidecar-Sperre von [`ConfigWriter`]: ein zweiter
//! Schreiber bekommt sofort [`ConfigError::Invalid`] („configuration busy“),
//! eine zwischenzeitlich geänderte Datei ergibt einen Konfliktfehler statt
//! eines stillen Überschreibens.
//!
//! # Fehler
//! [`ConfigError`]: `Invalid` für eine ungültige Modellreferenz, ein
//! fehlendes Home-Verzeichnis (nur [`set_dod_triage_model`]), eine
//! `[dod]`- oder `[dod.<tabelle>]`-Stelle, die keine Tabelle ist, oder eine
//! danach ungültige `[dod]`-Tabelle; `TomlParse`/`ReadFailed`/`Io` für
//! Lese- und Schreibfehler. `ReadFailed` und `Io` können Pfade enthalten:
//! Aufrufer loggen den Fehlertext daher höchstens auf `debug`.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//!
//! let home = Path::new("/home/user/.harw");
//! let _written: bool = harw_config::ensure_dod_defaults(home)?;
//! harw_config::set_dod_triage_model(home, Some("local/qwen2.5-coder"))?;
//! harw_config::set_dod_triage_model(home, None)?;
//! # Ok::<(), harw_config::ConfigError>(())
//! ```

use std::path::Path;

use serde::Deserialize;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::dod_toml::{DodSection, parse_triage_model};
use crate::error::{ConfigError, ConfigResult};
use crate::writer::ConfigWriter;

/// Dateiname der Home-Konfiguration unter `harw_home`.
const CONFIG_FILE: &str = "config.toml";

/// Setzt oder entfernt `[dod.triage] model` in `<harw_home>/config.toml`.
///
/// # Description
/// `Some(model)` wird zuerst getrimmt und muss dann
/// [`parse_triage_model`] genügen (`"<provider>/<model>"`); fehlende
/// Tabellen `[dod]`/`[dod.triage]` entstehen dabei, ein vorhandener Wert
/// wird mit seiner Formatierung ersetzt. `None` entfernt den Schlüssel (eine
/// leere `[dod.triage]`-Tabelle bleibt stehen, wie bei
/// [`ConfigWriter::remove_value`]); ohne Datei ist `None` ein No-op. Ob
/// Provider und Modell existieren, prüft diese Funktion nicht. Ein fehlendes
/// `harw_home` wird nie angelegt (ein vertippter `--home` soll keinen neuen
/// Root-Space erzeugen).
///
/// # Arguments
/// - `harw_home` (`&Path`): Root-Space (`~/.harw`), nicht das Profil.
/// - `model` (`Option<&str>`): neue Modellreferenz oder `None` zum Entfernen.
///
/// # Errors
/// - [`ConfigError::Invalid`]: ungültige Modellreferenz; `harw_home` ist
///   kein vorhandenes Verzeichnis (die Meldung nennt keinen Pfad); `dod`
///   oder `dod.triage` ist vorhanden, aber keine Tabelle; die resultierende
///   `[dod]`-Tabelle verletzt [`DodSection::validate`] (die Datei bleibt dann
///   unverändert); oder ein anderer Schreiber hält die Sperre bzw. hat die
///   Datei zwischenzeitlich geändert.
/// - [`ConfigError::TomlParse`], [`ConfigError::ReadFailed`],
///   [`ConfigError::Io`], [`ConfigError::CommittedDurabilityUncertain`]: wie
///   [`ConfigWriter::open`] bzw. [`ConfigWriter::save`].
///
/// # Concurrency
/// Blockierend; über die Sperre von [`ConfigWriter`] gegen parallele
/// Schreiber derselben Datei geschützt.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
///
/// let home = Path::new("/home/user/.harw");
/// harw_config::set_dod_triage_model(home, Some("openrouter/openai/gpt-4o"))?;
/// assert!(harw_config::set_dod_triage_model(home, Some("kein-provider")).is_err());
/// # Ok::<(), harw_config::ConfigError>(())
/// ```
pub fn set_dod_triage_model(harw_home: &Path, model: Option<&str>) -> ConfigResult<()> {
    let model = model.map(str::trim);
    if let Some(model) = model {
        parse_triage_model(model).map_err(ConfigError::Invalid)?;
    }
    if !harw_home.is_dir() {
        return Err(ConfigError::Invalid("harw home does not exist".to_owned()));
    }
    let path = harw_home.join(CONFIG_FILE);
    if model.is_none() && !path.exists() {
        return Ok(());
    }

    let mut writer = ConfigWriter::open(&path)?;
    let before = writer.document();
    let root = writer.document_mut().as_item_mut();
    match model {
        Some(model) => {
            let dod = ensure_child_table(root, "dod", "dod")?;
            let triage = ensure_child_table(dod, "triage", "dod.triage")?;
            set_child_value(triage, "model", Value::from(model), "dod.triage")?;
        }
        None => {
            remove_nested_value(root, &["dod", "triage", "model"]);
        }
    }
    let after = writer.document();
    if after == before {
        return Ok(());
    }
    validate_dod_document(&after)?;
    writer.save()
}

/// Ergänzt `[dod.approve] tui_tty = true` in einer vorhandenen
/// `<harw_home>/config.toml`, wenn der Schlüssel fehlt (Entscheidung O12).
///
/// # Description
/// - Ist `tui_tty` bereits gesetzt — gleich mit welchem Wert, also auch ein
///   ausdrückliches `false` —, bleibt die Datei unberührt.
/// - Fehlt der Schlüssel, entsteht er (samt fehlender Tabellen) neben allen
///   übrigen Inhalten und Kommentaren.
/// - Nichts passiert (`Ok(false)`, Grund auf `debug`), wenn `harw_home`
///   kein Verzeichnis ist, `config.toml` fehlt (die Datei legt das
///   Scaffolding von `harw-home` mit seiner vollständigen Vorlage an; ein
///   Eintrag hier würde diese Vorlage verdrängen) oder `config.toml` ein
///   Symlink bzw. keine reguläre Datei ist (der atomare Schreibpfad würde
///   den Symlink, etwa in ein Dotfiles-Repository, durch eine reguläre Datei
///   ersetzen). Die eingebaute Vorgabe ist ohnehin `true`.
///
/// Die Prüfung läuft zuerst nur lesend, sodass ein wiederholter Aufruf weder
/// eine Sperrdatei anlegt noch Rechte ändert; danach wird sie auf dem
/// Schnappschuss von [`ConfigWriter`] wiederholt, und
/// [`ConfigWriter::save`] lehnt einen inzwischen geänderten Stand ab.
///
/// # Arguments
/// - `harw_home` (`&Path`): Root-Space (`~/.harw`), nicht das Profil.
///
/// # Returns
/// `true`, wenn die Datei geschrieben wurde; `false`, wenn nichts zu tun war.
///
/// # Errors
/// - [`ConfigError::Invalid`]: `dod` oder `dod.approve` ist vorhanden, aber
///   keine Tabelle; die vorhandene `[dod]`-Tabelle ist ungültig (die Datei
///   bleibt dann unverändert); oder ein anderer Schreiber hält die Sperre
///   bzw. hat die Datei zwischenzeitlich geändert.
/// - [`ConfigError::TomlParse`], [`ConfigError::ReadFailed`],
///   [`ConfigError::Io`], [`ConfigError::CommittedDurabilityUncertain`]: wie
///   [`ConfigWriter::open`] bzw. [`ConfigWriter::save`].
///
/// # Concurrency
/// Blockierend; über die Sperre von [`ConfigWriter`] gegen parallele
/// Schreiber derselben Datei geschützt. Idempotent.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
///
/// let home = Path::new("/home/user/.harw");
/// let _first: bool = harw_config::ensure_dod_defaults(home)?;
/// // Ein zweiter Aufruf findet den Schlüssel (oder keine reguläre Datei)
/// // und schreibt nichts.
/// assert!(!harw_config::ensure_dod_defaults(home)?);
/// # Ok::<(), harw_config::ConfigError>(())
/// ```
pub fn ensure_dod_defaults(harw_home: &Path) -> ConfigResult<bool> {
    if !harw_home.is_dir() {
        tracing::debug!(
            reason = "harw_home_missing",
            "dod defaults: home directory absent, nothing written"
        );
        return Ok(false);
    }
    let path = harw_home.join(CONFIG_FILE);
    if let Some(reason) = backfill_skip_reason(&path)? {
        tracing::debug!(reason, "dod defaults: config.toml left untouched");
        return Ok(false);
    }
    let Some(content) = read_optional(&path)? else {
        tracing::debug!(
            reason = "config_absent",
            "dod defaults: config.toml left untouched"
        );
        return Ok(false);
    };
    let document = content
        .parse::<DocumentMut>()
        .map_err(|error| ConfigError::TomlParse(error.to_string()))?;
    if tui_tty_present(document.as_item())? {
        return Ok(false);
    }

    let mut writer = ConfigWriter::open(&path)?;
    // Repeated after opening the writer: the file may have been removed or
    // replaced by a symlink since the read-only check.
    if let Some(reason) = backfill_skip_reason(&path)? {
        tracing::debug!(reason, "dod defaults: config.toml left untouched");
        return Ok(false);
    }
    let before = writer.document();
    let root = writer.document_mut().as_item_mut();
    // Repeated on the writer's snapshot: another writer may have set the key
    // since; `save` refuses a snapshot that is stale by then.
    if tui_tty_present(root)? {
        return Ok(false);
    }
    let dod = ensure_child_table(root, "dod", "dod")?;
    let approve = ensure_child_table(dod, "approve", "dod.approve")?;
    set_child_value(approve, "tui_tty", Value::from(true), "dod.approve")?;
    let after = writer.document();
    if after == before {
        return Ok(false);
    }
    validate_dod_document(&after)?;
    writer.save()?;
    Ok(true)
}

// Grund, `path` beim Nachtragen der Vorgaben nicht anzufassen: fehlt, ist
// ein Symlink oder keine reguläre Datei. `None` heißt: reguläre Datei,
// nachtragen erlaubt. `symlink_metadata` folgt dem Symlink bewusst nicht.
fn backfill_skip_reason(path: &Path) -> ConfigResult<Option<&'static str>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(Some("config_is_symlink")),
        Ok(meta) if meta.is_file() => Ok(None),
        Ok(_) => Ok(Some("config_not_regular_file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some("config_absent")),
        Err(error) => Err(ConfigError::ReadFailed {
            path: path.display().to_string(),
            reason: error.to_string(),
        }),
    }
}

// Liest `path` als Text; `None`, wenn die Datei fehlt.
fn read_optional(path: &Path) -> ConfigResult<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ConfigError::ReadFailed {
            path: path.display().to_string(),
            reason: error.to_string(),
        }),
    }
}

// Ob `dod.approve.tui_tty` im Dokument steht (Wert egal). Fehler, wenn `dod`
// oder `dod.approve` vorhanden, aber keine Tabelle ist.
fn tui_tty_present(root: &Item) -> ConfigResult<bool> {
    let Some(dod) = child_table(root, "dod", "dod")? else {
        return Ok(false);
    };
    let Some(approve) = child_table(dod, "approve", "dod.approve")? else {
        return Ok(false);
    };
    Ok(approve
        .as_table_like()
        .is_some_and(|table| table.contains_key("tui_tty")))
}

// Die (Inline-)Tabelle `key` unter `parent`, falls vorhanden.
fn child_table<'a>(parent: &'a Item, key: &str, path: &str) -> ConfigResult<Option<&'a Item>> {
    let Some(table) = parent.as_table_like() else {
        return Ok(None);
    };
    match table.get(key) {
        None => Ok(None),
        Some(item) if item.is_table_like() => Ok(Some(item)),
        Some(_) => Err(not_a_table(path)),
    }
}

// Liefert die (Inline-)Tabelle `key` unter `parent` und legt sie bei Bedarf
// an: unter einer normalen Tabelle als implizite Tabelle (kein leerer
// `[dod]`-Kopf; unter einer Punkt-Tabelle wieder als Punkt-Tabelle), unter
// einer Inline-Tabelle als Inline-Tabelle. Ein vorhandener Nicht-Tabellen-
// Wert wird nie ersetzt, sondern abgelehnt.
fn ensure_child_table<'a>(
    parent: &'a mut Item,
    key: &str,
    path: &str,
) -> ConfigResult<&'a mut Item> {
    let fresh = match &*parent {
        Item::Table(table) => {
            let mut child = Table::new();
            child.set_implicit(true);
            child.set_dotted(table.is_dotted());
            Item::Table(child)
        }
        _ => Item::Value(Value::InlineTable(InlineTable::new())),
    };
    let table = parent
        .as_table_like_mut()
        .ok_or_else(|| ConfigError::WriterShapeMismatch {
            key: path.to_owned(),
            expected: "table",
        })?;
    if !table.contains_key(key) {
        table.insert(key, fresh);
    }
    let child = table
        .get_mut(key)
        .ok_or_else(|| ConfigError::WriterShapeMismatch {
            key: path.to_owned(),
            expected: "table",
        })?;
    if child.is_table_like() {
        Ok(child)
    } else {
        Err(not_a_table(path))
    }
}

// Setzt `key = new_value` in der (Inline-)Tabelle `parent`; ein vorhandener
// Wert behält seine Formatierung (Kommentare, Abstände).
fn set_child_value(
    parent: &mut Item,
    key: &str,
    mut new_value: Value,
    path: &str,
) -> ConfigResult<()> {
    let table = parent
        .as_table_like_mut()
        .ok_or_else(|| ConfigError::WriterShapeMismatch {
            key: path.to_owned(),
            expected: "table",
        })?;
    match table.get_mut(key) {
        Some(existing) => {
            if let Some(old) = existing.as_value() {
                *new_value.decor_mut() = old.decor().clone();
            }
            *existing = Item::Value(new_value);
        }
        None => {
            table.insert(key, Item::Value(new_value));
        }
    }
    Ok(())
}

// Entfernt den Wert unter `path` (Tabellen-Kette plus Blattschlüssel), falls
// vorhanden; leere Zwischentabellen bleiben stehen.
fn remove_nested_value(item: &mut Item, path: &[&str]) -> bool {
    let Some(table) = item.as_table_like_mut() else {
        return false;
    };
    match path {
        [] => false,
        [key] => table.remove(key).is_some(),
        [head, rest @ ..] => table
            .get_mut(head)
            .is_some_and(|child| remove_nested_value(child, rest)),
    }
}

// Fehler für eine `[dod]`-Stelle, die keine Tabelle ist.
fn not_a_table(path: &str) -> ConfigError {
    ConfigError::Invalid(format!(
        "{path} must be a table; fix the config file before harw edits it"
    ))
}

// Nur zur Validierung: liest ausschließlich `[dod]` aus dem gerenderten
// Dokument; alle anderen Top-Level-Schlüssel werden ignoriert.
#[derive(Deserialize)]
struct DodDocument {
    #[serde(default)]
    dod: DodSection,
}

// Prüft die `[dod]`-Tabelle des Kandidaten-Dokuments vor dem Schreiben
// (fail closed: bei einem Fehler bleibt die Datei unverändert).
fn validate_dod_document(document: &str) -> ConfigResult<()> {
    let parsed: DodDocument = toml::from_str(document)
        .map_err(|error| ConfigError::Invalid(format!("[dod]: {error}")))?;
    parsed.dod.validate().map_err(ConfigError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    // Liest `[dod]` aus `<home>/config.toml`.
    fn read_dod(home: &Path) -> TestResult<DodSection> {
        let content =
            std::fs::read_to_string(home.join(CONFIG_FILE)).map_err(ctx("config lesen"))?;
        let document: DodDocument = toml::from_str(&content).map_err(ctx("config parsen"))?;
        Ok(document.dod)
    }

    // Zählt die rotierten Backups `config.toml.bak.<n>` in `home`.
    fn backup_count(home: &Path) -> TestResult<usize> {
        let mut count = 0_usize;
        for entry in std::fs::read_dir(home).map_err(ctx("home lesen"))? {
            let entry = entry.map_err(ctx("Verzeichniseintrag"))?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("config.toml.bak."))
            {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    #[test]
    fn ensure_dod_defaults_backfills_an_existing_config_once() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join(CONFIG_FILE);
        std::fs::write(&path, "").map_err(ctx("leere config anlegen"))?;
        assert!(ensure_dod_defaults(home.path()).map_err(ctx("erster Aufruf"))?);
        let content = std::fs::read_to_string(&path).map_err(ctx("lesen"))?;
        assert!(content.contains("tui_tty = true"), "{content}");
        assert!(
            !content.contains("[dod]\n"),
            "no empty [dod] header: {content}"
        );
        assert!(read_dod(home.path())?.approve.tui_tty);

        assert!(!ensure_dod_defaults(home.path()).map_err(ctx("zweiter Aufruf"))?);
        let again = std::fs::read_to_string(&path).map_err(ctx("erneut lesen"))?;
        assert_eq!(again, content);
        Ok(())
    }

    #[test]
    fn ensure_dod_defaults_never_creates_a_missing_config() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        assert!(!ensure_dod_defaults(home.path()).map_err(ctx("ensure"))?);
        // Neither the config nor a lock sidecar: the scaffold template of
        // `harw-home` stays free to create the file later.
        assert!(!home.path().join(CONFIG_FILE).exists());
        assert!(!home.path().join("config.toml.lock").exists());
        Ok(())
    }

    #[test]
    fn ensure_dod_defaults_leaves_a_symlinked_config_alone() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dotfiles = tempfile::tempdir().map_err(ctx("dotfiles tempdir"))?;
        let target = dotfiles.path().join("harw-config.toml");
        let seed = "# aus dem Dotfiles-Repo\n[dod]\nauto_freeze = false\n";
        std::fs::write(&target, seed).map_err(ctx("ziel anlegen"))?;
        let link = home.path().join(CONFIG_FILE);
        std::os::unix::fs::symlink(&target, &link).map_err(ctx("symlink anlegen"))?;

        assert!(!ensure_dod_defaults(home.path()).map_err(ctx("ensure"))?);
        let meta = std::fs::symlink_metadata(&link).map_err(ctx("symlink lesen"))?;
        assert!(meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target).map_err(ctx("ziel lesen"))?,
            seed
        );
        assert!(!home.path().join("config.toml.lock").exists());

        // A directory in place of the file is left alone as well.
        let odd = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(odd.path().join(CONFIG_FILE)).map_err(ctx("verzeichnis anlegen"))?;
        assert!(!ensure_dod_defaults(odd.path()).map_err(ctx("ensure verzeichnis"))?);
        assert!(odd.path().join(CONFIG_FILE).is_dir());
        Ok(())
    }

    #[test]
    fn ensure_dod_defaults_preserves_comments_and_other_keys() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join(CONFIG_FILE);
        std::fs::write(
            &path,
            "# Kopfkommentar\ndefault_provider = \"local\"\n\n[dod]\n# DoD-Kommentar\nauto_freeze = false\n",
        )
        .map_err(ctx("config anlegen"))?;

        assert!(ensure_dod_defaults(home.path()).map_err(ctx("ensure"))?);
        let content = std::fs::read_to_string(&path).map_err(ctx("lesen"))?;
        assert!(content.contains("# Kopfkommentar"), "{content}");
        assert!(content.contains("# DoD-Kommentar"), "{content}");
        assert!(
            content.contains("default_provider = \"local\""),
            "{content}"
        );
        let dod = read_dod(home.path())?;
        assert!(!dod.auto_freeze);
        assert!(dod.approve.tui_tty);

        // Idempotent: the second call neither writes nor rotates a backup.
        let backups = backup_count(home.path())?;
        assert!(!ensure_dod_defaults(home.path()).map_err(ctx("zweiter Aufruf"))?);
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("erneut lesen"))?,
            content
        );
        assert_eq!(backup_count(home.path())?, backups);
        Ok(())
    }

    #[test]
    fn ensure_dod_defaults_keeps_an_explicit_false() -> TestResult {
        for seed in [
            "[dod.approve]\n# bewusst aus\ntui_tty = false\n",
            "dod = { approve = { tui_tty = false } }\n",
            "dod.approve.tui_tty = false\n",
        ] {
            let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let path = home.path().join(CONFIG_FILE);
            std::fs::write(&path, seed).map_err(ctx("config anlegen"))?;

            assert!(!ensure_dod_defaults(home.path()).map_err(ctx("ensure"))?);
            assert_eq!(std::fs::read_to_string(&path).map_err(ctx("lesen"))?, seed);
            assert!(!read_dod(home.path())?.approve.tui_tty);
            // The read-only pre-check leaves no lock file behind.
            assert!(!home.path().join("config.toml.lock").exists());
        }
        Ok(())
    }

    #[test]
    fn ensure_dod_defaults_extends_inline_and_dotted_tables() -> TestResult {
        for seed in [
            "dod = { auto_freeze = false }\n",
            "dod.auto_freeze = false\n",
            "[dod]\nauto_freeze = false\napprove = {}\n",
        ] {
            let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
            std::fs::write(home.path().join(CONFIG_FILE), seed).map_err(ctx("config anlegen"))?;

            assert!(ensure_dod_defaults(home.path()).map_err(ctx("ensure"))?);
            let dod = read_dod(home.path())?;
            assert!(!dod.auto_freeze, "seed {seed:?}");
            assert!(dod.approve.tui_tty, "seed {seed:?}");
        }
        Ok(())
    }

    #[test]
    fn ensure_dod_defaults_refuses_non_table_shapes_and_invalid_sections() -> TestResult {
        for seed in [
            "dod = 1\n",
            "[dod]\napprove = 1\n",
            "[dod]\nkill_requires_human = false\n",
        ] {
            let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let path = home.path().join(CONFIG_FILE);
            std::fs::write(&path, seed).map_err(ctx("config anlegen"))?;

            let Err(error) = ensure_dod_defaults(home.path()) else {
                return Err(TestError::Unexpected(format!(
                    "seed {seed:?} must be refused"
                )));
            };
            assert!(matches!(error, ConfigError::Invalid(_)), "{error}");
            assert_eq!(std::fs::read_to_string(&path).map_err(ctx("lesen"))?, seed);
            assert_eq!(backup_count(home.path())?, 0);
        }
        Ok(())
    }

    #[test]
    fn ensure_dod_defaults_without_home_directory_is_a_no_op() -> TestResult {
        let parent = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = parent.path().join("missing-home");
        assert!(!ensure_dod_defaults(&home).map_err(ctx("ensure"))?);
        assert!(!home.exists());
        Ok(())
    }

    #[test]
    fn set_dod_triage_model_writes_trims_and_preserves_comments() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join(CONFIG_FILE);
        std::fs::write(
            &path,
            "# Kopfkommentar\n[dod.triage]\n# Modellwahl\nmax_parallel = 3\n",
        )
        .map_err(ctx("config anlegen"))?;

        set_dod_triage_model(home.path(), Some("  local/qwen2.5-coder:7b "))
            .map_err(ctx("setzen"))?;
        let content = std::fs::read_to_string(&path).map_err(ctx("lesen"))?;
        assert!(content.contains("# Kopfkommentar"), "{content}");
        assert!(content.contains("# Modellwahl"), "{content}");
        let dod = read_dod(home.path())?;
        assert_eq!(dod.triage.model.as_deref(), Some("local/qwen2.5-coder:7b"));
        assert_eq!(dod.triage.max_parallel, 3);

        // Same value again: idempotent, no new backup.
        let backups = backup_count(home.path())?;
        set_dod_triage_model(home.path(), Some("local/qwen2.5-coder:7b"))
            .map_err(ctx("erneut setzen"))?;
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("erneut lesen"))?,
            content
        );
        assert_eq!(backup_count(home.path())?, backups);

        // Replacing keeps the rest of the table.
        set_dod_triage_model(home.path(), Some("openrouter/openai/gpt-4o"))
            .map_err(ctx("ersetzen"))?;
        let dod = read_dod(home.path())?;
        assert_eq!(
            dod.triage.model.as_deref(),
            Some("openrouter/openai/gpt-4o")
        );
        assert_eq!(dod.triage.max_parallel, 3);
        Ok(())
    }

    #[test]
    fn set_dod_triage_model_none_removes_only_the_model() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join(CONFIG_FILE);
        std::fs::write(
            &path,
            "[dod.triage]\nmodel = \"local/qwen2.5\"\nmax_parallel = 3\n",
        )
        .map_err(ctx("config anlegen"))?;

        set_dod_triage_model(home.path(), None).map_err(ctx("entfernen"))?;
        let dod = read_dod(home.path())?;
        assert_eq!(dod.triage.model, None);
        assert_eq!(dod.triage.max_parallel, 3);

        // Removing again is a no-op.
        let content = std::fs::read_to_string(&path).map_err(ctx("lesen"))?;
        set_dod_triage_model(home.path(), None).map_err(ctx("erneut entfernen"))?;
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("erneut lesen"))?,
            content
        );

        // Without a config file, `None` creates nothing.
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;
        set_dod_triage_model(empty.path(), None).map_err(ctx("ohne Datei"))?;
        assert!(!empty.path().join(CONFIG_FILE).exists());
        Ok(())
    }

    #[test]
    fn set_dod_triage_model_writes_into_inline_tables() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(
            home.path().join(CONFIG_FILE),
            "dod = { auto_freeze = false }\n",
        )
        .map_err(ctx("config anlegen"))?;

        set_dod_triage_model(home.path(), Some("local/qwen2.5")).map_err(ctx("setzen"))?;
        let dod = read_dod(home.path())?;
        assert!(!dod.auto_freeze);
        assert_eq!(dod.triage.model.as_deref(), Some("local/qwen2.5"));
        Ok(())
    }

    #[test]
    fn set_dod_triage_model_never_creates_a_missing_home() -> TestResult {
        let parent = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = parent.path().join("vertippt");
        for model in [Some("local/qwen2.5"), None] {
            let Err(error) = set_dod_triage_model(&home, model) else {
                return Err(TestError::Unexpected(format!(
                    "{model:?} on a missing home must be refused"
                )));
            };
            assert!(matches!(error, ConfigError::Invalid(_)), "{error}");
            // The message names no path.
            assert!(!error.to_string().contains("vertippt"), "{error}");
            assert!(!home.exists());
        }
        Ok(())
    }

    #[test]
    fn set_dod_triage_model_rejects_invalid_shapes_and_leaves_the_file_unchanged() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join(CONFIG_FILE);
        let seed = "# Kommentar\n[dod.triage]\nmodel = \"local/qwen2.5\"\n";
        std::fs::write(&path, seed).map_err(ctx("config anlegen"))?;

        for bad in [
            "noslash",
            "/model",
            "provider/",
            "pro vider/model",
            "",
            "p//m",
            "../x",
            "p/../../etc",
        ] {
            let Err(error) = set_dod_triage_model(home.path(), Some(bad)) else {
                return Err(TestError::Unexpected(format!("{bad:?} must be rejected")));
            };
            assert!(matches!(error, ConfigError::Invalid(_)), "{error}");
            assert_eq!(std::fs::read_to_string(&path).map_err(ctx("lesen"))?, seed);
        }

        // An invalid existing `[dod]` is not written over either.
        std::fs::write(&path, "[dod]\nkill_requires_human = false\n")
            .map_err(ctx("ungültige config anlegen"))?;
        let Err(error) = set_dod_triage_model(home.path(), Some("local/qwen2.5")) else {
            return Err(TestError::Unexpected(
                "an invalid [dod] table must not be written over".to_owned(),
            ));
        };
        assert!(matches!(error, ConfigError::Invalid(_)), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("erneut lesen"))?,
            "[dod]\nkill_requires_human = false\n"
        );
        assert_eq!(backup_count(home.path())?, 0);
        Ok(())
    }
}
