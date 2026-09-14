//! `ConfigWriter` — editiert genau eine `config.toml` mit `toml_edit` und
//! erhält dabei Kommentare/Formatierung (Contract
//! `harw-scopes-contract.md` §5 Zeile A2; Plan
//! `nope-permissions-gibt-es-wild-lobster.md` Schritt 4).
//!
//! # Verantwortung
//! Dieses Modul kennt nur das `[permissions]`-Schema
//! ([`PermissionsSection`], [`RuleToml`]) sowie die generische
//! `set_value`/`get_value`-Punktnotation für beliebige Schlüssel. Es besitzt
//! die Backup-Rotation und den atomaren Schreibpfad; das Muster stammt aus
//! `harw-install/src/migration.rs:61-267` (Parse → Backup → Ändern →
//! Schreiben).
//!
//! # Fehlersemantik
//! [`ConfigWriter::save`] validiert das Dokument **vor** dem Schreiben
//! (fail closed): schlägt die Re-Deserialisierung in [`PermissionsSection`]
//! oder deren [`PermissionsSection::validate`] fehl, bleibt die Datei auf
//! der Platte unverändert.
//!
//! # Nebenläufigkeit
//! Ein [`ConfigWriter`] hält das Dokument exklusiv im Prozessspeicher, bis
//! `save()` läuft. Parallele Schreiber auf denselben Pfad sind nicht
//! abgesichert (keine Dateisperren), analog zu `MigrationRunner`.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value, value};

use crate::error::{ConfigError, ConfigResult};
use crate::permissions_toml::{PermissionsSection, RuleToml};

/// Höchste Anzahl aufbewahrter Backups (`<datei>.bak.<n>`); ältere werden
/// nach jedem `save()` gelöscht.
const MAX_BACKUPS: usize = 5;

/// Wählt, in welche Regelliste (`[[permissions.allow]]` oder
/// `[[permissions.deny]]`) `append_rule`/`remove_rule` schreiben.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// `[[permissions.allow]]`.
    Allow,
    /// `[[permissions.deny]]`.
    Deny,
}

impl RuleKind {
    /// TOML-Schlüssel innerhalb von `[permissions]` (`"allow"`/`"deny"`).
    fn table_key(self) -> &'static str {
        match self {
            RuleKind::Allow => "allow",
            RuleKind::Deny => "deny",
        }
    }
}

/// Editiert eine einzelne TOML-Datei unter `[permissions]`.
///
/// # Description
/// Hält den Zielpfad und das geparste [`DocumentMut`] im Speicher. Alle
/// `set_*`/`append_*`/`remove_*`-Methoden mutieren nur das In-Memory-Dokument;
/// erst [`ConfigWriter::save`] validiert und persistiert.
#[derive(Debug)]
pub struct ConfigWriter {
    path: PathBuf,
    doc: DocumentMut,
}

impl ConfigWriter {
    /// Öffnet `path` zur Bearbeitung.
    ///
    /// # Description
    /// Fehlt die Datei, entsteht ein leeres Dokument. Das Elternverzeichnis
    /// wird bei Bedarf angelegt und auf `0700` gesetzt (analog
    /// `harw-home/src/trust.rs`).
    ///
    /// # Errors
    /// - [`ConfigError::TomlParse`]: vorhandener Inhalt ist kein gültiges TOML.
    /// - [`ConfigError::ReadFailed`]: Lesefehler außer „nicht gefunden“.
    /// - [`ConfigError::Io`]: das Elternverzeichnis konnte nicht angelegt
    ///   oder nicht auf `0700` gesetzt werden.
    pub fn open(path: &Path) -> ConfigResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                ensure_private_dir(parent)?;
            }
        }
        let doc = match fs::read_to_string(path) {
            Ok(content) => content
                .parse::<DocumentMut>()
                .map_err(|err| ConfigError::TomlParse(err.to_string()))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => DocumentMut::new(),
            Err(source) => {
                return Err(ConfigError::ReadFailed {
                    path: path.display().to_string(),
                    reason: source.to_string(),
                });
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            doc,
        })
    }

    /// Setzt `[permissions] default_mode`.
    ///
    /// # Arguments
    /// - `mode` (`&str`): sollte `ask`, `auto` oder `full` sein;
    ///   `PermissionsSection::validate` prüft dies erst bei `save()`.
    pub fn set_default_mode(&mut self, mode: &str) {
        self.permissions_table().insert("default_mode", value(mode));
    }

    /// Setzt `[permissions] approval_timeout_secs`.
    pub fn set_approval_timeout(&mut self, secs: u64) {
        let as_i64 = i64::try_from(secs).unwrap_or(i64::MAX);
        self.permissions_table()
            .insert("approval_timeout_secs", value(as_i64));
    }

    /// Hängt `rule` an `[[permissions.allow]]` bzw. `[[permissions.deny]]` an.
    ///
    /// # Returns
    /// `true`, wenn die Regel neu angehängt wurde; `false`, wenn eine
    /// identische Regel (`tool` **und** `pattern`) bereits vorhanden war
    /// (kein Duplikat, keine Änderung).
    pub fn append_rule(&mut self, kind: RuleKind, rule: &RuleToml) -> bool {
        let array = self.rule_array(kind);
        if rule_array_contains(array, rule) {
            return false;
        }
        array.push(rule_to_table(rule));
        true
    }

    /// Entfernt die Regel an `index` aus `[[permissions.allow]]` bzw.
    /// `[[permissions.deny]]`.
    ///
    /// # Returns
    /// Die entfernte Regel, oder `None`, wenn `index` außerhalb der aktuellen
    /// Länge liegt (keine Änderung).
    pub fn remove_rule(&mut self, kind: RuleKind, index: usize) -> Option<RuleToml> {
        let array = self.rule_array(kind);
        if index >= array.len() {
            return None;
        }
        Some(table_to_rule(&array.remove(index)))
    }

    /// Hängt `root` an `[permissions] extra_roots` an.
    ///
    /// # Returns
    /// `true`, wenn der Pfad neu angehängt wurde; `false`, wenn er bereits
    /// enthalten war.
    pub fn append_extra_root(&mut self, root: &Path) -> bool {
        let root_str = root.to_string_lossy().into_owned();
        let array = self.extra_roots_array();
        if array_contains_str(array, &root_str) {
            return false;
        }
        array.push(root_str);
        true
    }

    /// Entfernt `root` aus `[permissions] extra_roots`.
    ///
    /// # Returns
    /// `true`, wenn der Pfad entfernt wurde; `false`, wenn er nicht
    /// enthalten war.
    pub fn remove_extra_root(&mut self, root: &Path) -> bool {
        let root_str = root.to_string_lossy().into_owned();
        let array = self.extra_roots_array();
        match array_index_of_str(array, &root_str) {
            Some(index) => {
                array.remove(index);
                true
            }
            None => false,
        }
    }

    /// Setzt einen beliebigen Wert über einen punktgetrennten Schlüsselpfad
    /// (z. B. `"permissions.default_mode"`). Zwischentabellen entstehen bei
    /// Bedarf; ein Nicht-Tabellen-Wert auf dem Pfad wird durch eine leere
    /// Tabelle ersetzt.
    pub fn set_value(&mut self, dotted_key: &str, item: Item) {
        let segments: Vec<&str> = dotted_key.split('.').collect();
        set_value_in_table(self.doc.as_table_mut(), &segments, item);
    }

    /// Liest einen Wert über einen punktgetrennten Schlüsselpfad als
    /// menschenlesbaren String.
    ///
    /// # Returns
    /// `Some(text)` für einen skalaren Wert (String unquotiert, sonst die
    /// TOML-Textform); `None`, wenn der Pfad fehlt oder auf eine Tabelle/ein
    /// Array zeigt.
    #[must_use]
    pub fn get_value(&self, dotted_key: &str) -> Option<String> {
        let mut item: &Item = self.doc.as_item();
        for segment in dotted_key.split('.') {
            item = item.as_table().and_then(|table| table.get(segment))?;
        }
        match item {
            Item::Value(value) => Some(value_to_display_string(value)),
            _ => None,
        }
    }

    /// Validiert das Dokument gegen [`PermissionsSection`] und schreibt es
    /// danach atomar zurück; ein Backup wird vorher rotiert.
    ///
    /// # Errors
    /// - [`ConfigError::TomlParse`]: das gerenderte Dokument ist nicht mehr
    ///   valides TOML (sollte bei ausschließlicher Nutzung dieser API nicht
    ///   auftreten, da `toml_edit` stets syntaktisch gültiges TOML erzeugt).
    /// - [`ConfigError::Invalid`]: die resultierende `[permissions]`-Sektion
    ///   verletzt [`PermissionsSection::validate`] — die Datei bleibt
    ///   unverändert (fail closed).
    /// - [`ConfigError::Io`]: Backup oder atomares Schreiben schlugen fehl.
    pub fn save(&self) -> ConfigResult<()> {
        let rendered = self.doc.to_string();

        let parsed: PermissionsOnlyDocument = toml::from_str(&rendered)
            .map_err(|err| ConfigError::TomlParse(err.to_string()))?;
        parsed.permissions.validate().map_err(ConfigError::Invalid)?;

        rotate_backups(&self.path)?;
        harw_fsutil::write_atomic(
            &self.path,
            rendered.as_bytes(),
            harw_fsutil::AtomicWriteOptions::with_mode(0o600),
        )?;
        Ok(())
    }

    /// Liefert `[permissions]` als veränderliche Tabelle, legt sie bei
    /// Bedarf an.
    fn permissions_table(&mut self) -> &mut Table {
        ensure_table(self.doc.as_table_mut(), "permissions")
    }

    /// Liefert `[[permissions.allow]]`/`[[permissions.deny]]` als
    /// veränderliches Array-of-Tables, legt es bei Bedarf an.
    fn rule_array(&mut self, kind: RuleKind) -> &mut ArrayOfTables {
        let permissions = ensure_table(self.doc.as_table_mut(), "permissions");
        ensure_array_of_tables(permissions, kind.table_key())
    }

    /// Liefert `[permissions] extra_roots` als veränderliches Array, legt es
    /// bei Bedarf an.
    fn extra_roots_array(&mut self) -> &mut Array {
        let permissions = ensure_table(self.doc.as_table_mut(), "permissions");
        ensure_array(permissions, "extra_roots")
    }
}

/// Nur zur Validierung vor `save()`: liest ausschließlich `[permissions]`
/// aus dem vollständigen gerenderten Dokument; alle anderen Top-Level-Keys
/// werden ignoriert (kein `deny_unknown_fields`).
#[derive(Debug, Deserialize)]
struct PermissionsOnlyDocument {
    #[serde(default)]
    permissions: PermissionsSection,
}

/// Stellt sicher, dass `table[key]` eine Tabelle ist (legt sie an oder
/// ersetzt einen Nicht-Tabellen-Wert), und gibt eine veränderliche Referenz
/// darauf zurück.
fn ensure_table<'a>(table: &'a mut Table, key: &str) -> &'a mut Table {
    if !matches!(table.get(key), Some(item) if item.is_table()) {
        table.insert(key, Item::Table(Table::new()));
    }
    match table.get_mut(key).and_then(Item::as_table_mut) {
        Some(t) => t,
        None => unreachable!("key {key:?} was just normalised to a table"),
    }
}

/// Stellt sicher, dass `table[key]` ein Array-of-Tables ist, und gibt eine
/// veränderliche Referenz darauf zurück.
fn ensure_array_of_tables<'a>(table: &'a mut Table, key: &str) -> &'a mut ArrayOfTables {
    if !matches!(table.get(key), Some(item) if item.is_array_of_tables()) {
        table.insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    match table.get_mut(key).and_then(Item::as_array_of_tables_mut) {
        Some(a) => a,
        None => unreachable!("key {key:?} was just normalised to an array of tables"),
    }
}

/// Stellt sicher, dass `table[key]` ein Inline-Array ist, und gibt eine
/// veränderliche Referenz darauf zurück.
fn ensure_array<'a>(table: &'a mut Table, key: &str) -> &'a mut Array {
    if !matches!(table.get(key), Some(item) if item.is_array()) {
        table.insert(key, Item::Value(Value::Array(Array::new())));
    }
    match table.get_mut(key).and_then(Item::as_array_mut) {
        Some(a) => a,
        None => unreachable!("key {key:?} was just normalised to an array"),
    }
}

/// Setzt `item` rekursiv unter `segments` in `table` (letztes Segment =
/// Blattschlüssel); Zwischensegmente werden über [`ensure_table`] normalisiert.
fn set_value_in_table(table: &mut Table, segments: &[&str], item: Item) {
    match segments {
        [] => {}
        [last] => {
            table.insert(last, item);
        }
        [head, rest @ ..] => {
            let child = ensure_table(table, head);
            set_value_in_table(child, rest, item);
        }
    }
}

/// Baut eine `RuleToml` in eine `toml_edit::Table` (`tool`, optional
/// `pattern`) um.
fn rule_to_table(rule: &RuleToml) -> Table {
    let mut table = Table::new();
    table.insert("tool", value(rule.tool.as_str()));
    if let Some(pattern) = &rule.pattern {
        table.insert("pattern", value(pattern.as_str()));
    }
    table
}

/// Liest eine `RuleToml` aus einer `toml_edit::Table` zurück; ein fehlendes
/// `tool` wird als leerer String gelesen (Validierung erfolgt separat über
/// [`RuleToml::validate`]).
fn table_to_rule(table: &Table) -> RuleToml {
    RuleToml {
        tool: table
            .get("tool")
            .and_then(Item::as_str)
            .unwrap_or_default()
            .to_owned(),
        pattern: table.get("pattern").and_then(Item::as_str).map(str::to_owned),
    }
}

/// `true`, wenn `array` bereits eine Regel mit identischem `tool` **und**
/// `pattern` enthält.
fn rule_array_contains(array: &ArrayOfTables, rule: &RuleToml) -> bool {
    array.iter().any(|table| {
        let existing = table_to_rule(table);
        existing.tool == rule.tool && existing.pattern == rule.pattern
    })
}

/// `true`, wenn `array` den String `needle` als Element enthält.
fn array_contains_str(array: &Array, needle: &str) -> bool {
    array.iter().any(|v| v.as_str() == Some(needle))
}

/// Index von `needle` in `array`, falls vorhanden.
fn array_index_of_str(array: &Array, needle: &str) -> Option<usize> {
    array.iter().position(|v| v.as_str() == Some(needle))
}

/// Menschenlesbare Textform eines `toml_edit::Value` (Strings unquotiert,
/// alle anderen Typen über ihre TOML-Textform).
fn value_to_display_string(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_owned();
    }
    value.to_string().trim().to_owned()
}

/// Legt `dir` an (rekursiv) und setzt Rechte `0700`, analog
/// `harw-home/src/trust.rs`.
///
/// # Errors
/// - [`ConfigError::Io`]: Anlegen oder Rechtesetzen schlug fehl.
fn ensure_private_dir(dir: &Path) -> ConfigResult<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Kopiert `path` (falls vorhanden) nach `<path>.bak.<n>` mit der
/// nächsthöheren, bisher unbenutzten Nummer, und löscht danach die
/// ältesten Backups, sofern mehr als [`MAX_BACKUPS`] existieren.
///
/// # Errors
/// - [`ConfigError::Io`]: Lesen der Quelldatei, Schreiben des Backups oder
///   Verzeichnis-Scan schlugen fehl.
fn rotate_backups(path: &Path) -> ConfigResult<()> {
    let content = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(ConfigError::from(source)),
    };

    let mut numbers = collect_backup_numbers(path)?;
    let next = numbers.iter().max().map_or(1, |max| max.saturating_add(1));
    fs::write(backup_candidate(path, next), &content)?;
    numbers.push(next);

    numbers.sort_unstable();
    while numbers.len() > MAX_BACKUPS {
        let oldest = numbers.remove(0);
        // Best effort: ein bereits fehlendes altes Backup ist kein Fehler.
        let _ = fs::remove_file(backup_candidate(path, oldest));
    }
    Ok(())
}

/// Baut den Backup-Pfad `<path>.bak.<n>` für eine gegebene Nummer.
fn backup_candidate(path: &Path, n: u32) -> PathBuf {
    let mut name = path.file_name().map(|f| f.to_owned()).unwrap_or_default();
    name.push(format!(".bak.{n}"));
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Sammelt die Nummern aller vorhandenen `<dateiname>.bak.<n>`-Einträge im
/// Elternverzeichnis von `path`.
fn collect_backup_numbers(path: &Path) -> ConfigResult<Vec<u32>> {
    let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
        return Ok(Vec::new());
    };
    let dir: PathBuf = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let prefix = format!("{file_name}.bak.");

    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ConfigError::from(source)),
    };

    let mut numbers = Vec::new();
    for entry in entries {
        let entry = entry.map_err(ConfigError::from)?;
        if let Some(name) = entry.file_name().to_str() {
            if let Some(suffix) = name.strip_prefix(&prefix) {
                if let Ok(n) = suffix.parse::<u32>() {
                    numbers.push(n);
                }
            }
        }
    }
    Ok(numbers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        dir.path().join(name)
    }

    #[test]
    fn test_open_missing_file_creates_private_parent_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested").join("config.toml");

        let writer = ConfigWriter::open(&nested).expect("open missing file");
        assert!(!nested.exists());
        let parent_mode = fs::metadata(nested.parent().expect("parent"))
            .expect("parent metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700);
        assert!(writer.get_value("permissions.default_mode").is_none());
    }

    #[test]
    fn test_set_default_mode_and_save_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");

        let mut writer = ConfigWriter::open(&path).expect("open");
        writer.set_default_mode("auto");
        writer.save().expect("save");

        let reopened = ConfigWriter::open(&path).expect("reopen");
        assert_eq!(
            reopened.get_value("permissions.default_mode"),
            Some("auto".to_owned())
        );
    }

    #[test]
    fn test_save_preserves_existing_comment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        fs::write(
            &path,
            "# wichtiger Kommentar\ndefault_provider = \"anthropic\"\n",
        )
        .expect("seed file");

        let mut writer = ConfigWriter::open(&path).expect("open");
        writer.set_default_mode("ask");
        writer.save().expect("save");

        let content = fs::read_to_string(&path).expect("read back");
        assert!(content.contains("# wichtiger Kommentar"));
        assert!(content.contains("default_provider = \"anthropic\""));
        assert!(content.contains("default_mode = \"ask\""));
    }

    #[test]
    fn test_save_creates_first_backup_as_bak_1() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        fs::write(&path, "default_provider = \"anthropic\"\n").expect("seed file");

        let mut writer = ConfigWriter::open(&path).expect("open");
        writer.set_default_mode("ask");
        writer.save().expect("save");

        assert!(backup_candidate(&path, 1).exists());
    }

    #[test]
    fn test_save_keeps_only_five_newest_backups() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        fs::write(&path, "default_provider = \"anthropic\"\n").expect("seed file");

        for i in 0..7 {
            let mut writer = ConfigWriter::open(&path).expect("open");
            writer.set_approval_timeout(10 + i);
            writer.save().expect("save");
        }

        for n in 1..=2 {
            assert!(!backup_candidate(&path, n).exists(), "bak.{n} should be pruned");
        }
        for n in 3..=7 {
            assert!(backup_candidate(&path, n).exists(), "bak.{n} should remain");
        }
    }

    #[test]
    fn test_append_rule_dedupes_identical_rules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        let mut writer = ConfigWriter::open(&path).expect("open");

        let rule = RuleToml {
            tool: "shell.exec".to_owned(),
            pattern: Some("cargo check".to_owned()),
        };
        assert!(writer.append_rule(RuleKind::Allow, &rule));
        assert!(!writer.append_rule(RuleKind::Allow, &rule));
        writer.save().expect("save");

        let content = fs::read_to_string(&path).expect("read back");
        assert_eq!(content.matches("cargo check").count(), 1);
    }

    #[test]
    fn test_remove_rule_returns_removed_value_and_shifts_indices() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        let mut writer = ConfigWriter::open(&path).expect("open");

        writer.append_rule(
            RuleKind::Deny,
            &RuleToml {
                tool: "fs.write".to_owned(),
                pattern: None,
            },
        );
        writer.append_rule(
            RuleKind::Deny,
            &RuleToml {
                tool: "fs.delete".to_owned(),
                pattern: None,
            },
        );

        let removed = writer.remove_rule(RuleKind::Deny, 0).expect("removed rule");
        assert_eq!(removed.tool, "fs.write");
        assert!(writer.remove_rule(RuleKind::Deny, 5).is_none());

        writer.save().expect("save");
        let content = fs::read_to_string(&path).expect("read back");
        assert!(!content.contains("fs.write"));
        assert!(content.contains("fs.delete"));
    }

    #[test]
    fn test_append_and_remove_extra_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        let mut writer = ConfigWriter::open(&path).expect("open");

        let root = Path::new("/home/mia/scratch");
        assert!(writer.append_extra_root(root));
        assert!(!writer.append_extra_root(root));
        writer.save().expect("save");

        let reopened_content = fs::read_to_string(&path).expect("read back");
        assert!(reopened_content.contains("/home/mia/scratch"));

        let mut writer = ConfigWriter::open(&path).expect("reopen");
        assert!(writer.remove_extra_root(root));
        assert!(!writer.remove_extra_root(root));
        writer.save().expect("save after removal");

        let final_content = fs::read_to_string(&path).expect("read back again");
        assert!(!final_content.contains("/home/mia/scratch"));
    }

    #[test]
    fn test_invalid_mode_leaves_file_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        fs::write(&path, "default_provider = \"anthropic\"\n").expect("seed file");
        let before = fs::read_to_string(&path).expect("read before");

        let mut writer = ConfigWriter::open(&path).expect("open");
        writer.set_default_mode("yolo");
        let result = writer.save();
        assert!(result.is_err());

        let after = fs::read_to_string(&path).expect("read after failed save");
        assert_eq!(before, after);
        assert!(!backup_candidate(&path, 1).exists());
    }

    #[test]
    fn test_set_value_and_get_value_dotted_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        let mut writer = ConfigWriter::open(&path).expect("open");

        writer.set_value("workspace.extra_roots_hint", value("/tmp/example"));
        assert_eq!(
            writer.get_value("workspace.extra_roots_hint"),
            Some("/tmp/example".to_owned())
        );
        assert!(writer.get_value("workspace.missing").is_none());
        assert!(writer.get_value("missing.entirely").is_none());
    }

    #[test]
    fn test_set_value_replaces_non_table_intermediate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_config_path(&dir, "config.toml");
        fs::write(&path, "permissions = \"not-a-table\"\n").expect("seed file");

        let mut writer = ConfigWriter::open(&path).expect("open");
        writer.set_default_mode("ask");
        assert_eq!(
            writer.get_value("permissions.default_mode"),
            Some("ask".to_owned())
        );
    }
}
