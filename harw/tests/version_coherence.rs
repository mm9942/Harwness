//! Slice 14 — Workspace version coherence integration test.
//!
//! Spec source: "Workspace-Versionen, interne Dependency-Versionen und
//! Lockfile sind konsistent." (0.3.0; Slice 14).
//!
//! Asserts:
//!   1. Every workspace-owned package (`harw` or `harw-*`) reports version
//!      `0.3.0` (resolved from its own `Cargo.toml`).
//!   2. No workspace package is still on `0.1.0`.
//!   3. `Cargo.lock` confirms every internal package resolves to `0.3.0`.
//!   4. The workspace root `Cargo.toml` contains the version string exactly
//!      once; no member `Cargo.toml` contains it at all (they inherit via
//!      `version.workspace = true`).
//!   5. No member `Cargo.toml` contains a bare `version = "0.1.0"` pin.
//!
//! # Befund G-080 (Korrektur)
//! Diese Datei rief bis zu dieser Korrektur `cargo_metadata::MetadataCommand`
//! auf, das seinerseits einen `cargo metadata`-Subprozess startet. In der
//! sandboxten Remediation-Umgebung (kein `cargo` erlaubt, siehe
//! `xtask`-Auftragsregeln) scheitert dieser Subprozess-Aufruf, und der Test
//! ist seitdem dauerhaft rot — unabhängig davon, ob die Workspace-Versionen
//! tatsächlich kohärent sind. Ein Test, der aus einem falschen Grund rot ist,
//! verdeckt den echten Zustand, den er eigentlich prüfen soll.
//!
//! **Korrektur:** Alle fünf Aussagen werden jetzt durch **statisches**
//! Parsen der Workspace-Root-`Cargo.toml`, jedes Mitglieds-`Cargo.toml` und
//! der `Cargo.lock` mit dem `toml`-Crate gewonnen — kein Subprozess, kein
//! `cargo`-Aufruf. `Cargo.lock` ist selbst gültiges TOML (eine Folge von
//! `[[package]]`-Tabellen mit `name`/`version`), weshalb derselbe Parser für
//! alle drei Dateien genügt.
//!
//! # Grenzen dieser Auflösung — bewusst nicht geschlossen
//! - `[workspace.members]` wird nur als Liste **wörtlicher** Verzeichnis-
//!   namen gelesen, kein Glob-Muster (`"crates/*"`). Die Wurzel-`Cargo.toml`
//!   dieses Workspace verwendet ausschließlich wörtliche Einträge, siehe
//!   dort.
//! - Ein Umbenennungs-Alias oder ein Patch-Abschnitt wird nicht ausgewertet
//!   — für die hier geprüfte Frage (eigene Paketversion je Mitglied) spielt
//!   das keine Rolle.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const EXPECTED_VERSION: &str = "0.3.0";
const OLD_VERSION: &str = "0.1.0";

/// Walk up from `start` until a directory contains a `Cargo.toml` that has
/// the `[workspace]` table.  Returns the path to that directory.
///
/// # Panics
/// Panics if no workspace root is found before reaching the filesystem root.
fn find_workspace_root(start: &Path) -> PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            let text = fs::read_to_string(&candidate).unwrap_or_default();
            if text.contains("[workspace]") {
                return dir;
            }
        }
        if !dir.pop() {
            panic!("Could not find workspace root above {}", start.display());
        }
    }
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

/// Liest und parst eine TOML-Datei (Manifest oder Lockfile — beide sind
/// gültiges TOML). Panic mit Pfadangabe statt eines nackten `unwrap`, damit
/// ein Testfehlschlag sofort zeigt, welche Datei betroffen ist.
///
/// # API-Nachweis (R3-01-Korrektur)
/// `text.parse::<toml::Value>()` parst laut Registry-Quelltext **einen
/// einzelnen TOML-Wert, kein Dokument**: `impl FromStr for Value`
/// (`toml-1.1.3+spec-1.1.0/src/value.rs:394-400`) ruft
/// `crate::de::ValueDeserializer::parse(s)` auf
/// (`.../src/de/deserializer/value.rs:44-50`), das wiederum
/// `DeValue::parse` nutzt, dessen Doc-Kommentar „Parse a TOML value“ lautet
/// (`.../src/de/parser/devalue.rs:138-139`). Ein Dokument mit mehreren
/// Top-Level-Tabellen (`[package]`, `[workspace]`, `[[package]]`, …) ist
/// **kein** einzelner Wert und scheitert dort.
///
/// Dokumente werden stattdessen über `toml::Table: FromStr`
/// (`.../src/table.rs:53-58`, delegiert an `crate::from_str`,
/// `.../src/de/mod.rs:72`) geparst — das entspricht dem Crate-Beispiel
/// `"foo = 'bar'".parse::<Table>()` in `.../src/lib.rs:38-41`. Das Ergebnis
/// wird in `toml::Value::Table(..)` gewickelt, damit die bestehenden
/// `Value::get`/`as_*`-Aufrufe unverändert bleiben.
fn read_toml(path: &Path) -> toml::Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    let table = text
        .parse::<toml::Table>()
        .unwrap_or_else(|e| panic!("Cannot parse {} as TOML: {e}", path.display()));
    toml::Value::Table(table)
}

/// Liest `[workspace.members]` aus der bereits geparsten Wurzel-`Cargo.toml`
/// als Liste von Mitgliedsverzeichnissen (relativ zu `workspace_root`
/// aufgelöst).
///
/// # API-Nachweis
/// `Value::get<I: Index>(&self, index: I) -> Option<&Self>` (Index u. a. für
/// `&str` implementiert) sowie `Value::as_array(&self) -> Option<&Vec<Self>>`
/// und `Value::as_str(&self) -> Option<&str>` — siehe
/// docs.rs/toml/1.1.3/toml/enum.Value.html, Abschnitt "Value Extraction
/// Methods" / "Indexing Methods".
fn workspace_member_dirs(root_value: &toml::Value, workspace_root: &Path) -> Vec<PathBuf> {
    let members = root_value
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .expect("[workspace] members muss ein Array sein");

    members
        .iter()
        .map(|m| {
            let name = m
                .as_str()
                .unwrap_or_else(|| panic!("[workspace.members]-Eintrag ist kein String: {m:?}"));
            workspace_root.join(name)
        })
        .collect()
}

/// Wie ein Mitglied seine eigene Paketversion deklariert.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionDecl {
    /// `version = "…"` als wörtliche Zeichenkette.
    Literal(String),
    /// `version.workspace = true` — geerbt von `[workspace.package.version]`.
    WorkspaceInherited,
    /// Weder das eine noch das andere (fehlendes oder unerwartetes Feld).
    Missing,
}

/// Wertet das `[package] version`-Feld eines bereits geparsten Manifests aus.
///
/// # API-Nachweis
/// `toml::Table = Map<String, Value>` (Typalias, docs.rs/toml/1.1.3/toml/
/// type.Table.html); `Value::as_bool(&self) -> Option<bool>` — siehe
/// docs.rs/toml/1.1.3/toml/enum.Value.html.
fn parse_version_decl(package: &toml::Value) -> VersionDecl {
    match package.get("version") {
        Some(toml::Value::String(s)) => VersionDecl::Literal(s.clone()),
        Some(toml::Value::Table(t)) if t.get("workspace").and_then(toml::Value::as_bool) == Some(true) => {
            VersionDecl::WorkspaceInherited
        }
        _ => VersionDecl::Missing,
    }
}

/// Ein einzelnes, bereits gelesenes Workspace-Mitglied, eingeschränkt auf
/// das, was diese Datei braucht.
struct MemberManifest {
    name: String,
    path: PathBuf,
    version_decl: VersionDecl,
}

#[test]
fn slice14_workspace_version_is_consistent() {
    // ── Locate workspace root ────────────────────────────────────────────
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = find_workspace_root(&manifest_dir);

    // ── Parse the workspace root Cargo.toml statically (no `cargo`) ───────
    let root_toml_path = workspace_root.join("Cargo.toml");
    let root_value = read_toml(&root_toml_path);

    let root_workspace_version = root_value
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .expect("[workspace.package.version] fehlt oder ist kein String")
        .to_owned();

    // ── Parse every workspace member's own Cargo.toml ─────────────────────
    let member_dirs = workspace_member_dirs(&root_value, &workspace_root);
    let mut all_members: Vec<MemberManifest> = Vec::new();
    for dir in &member_dirs {
        let path = dir.join("Cargo.toml");
        let value = read_toml(&path);
        let package = value
            .get("package")
            .unwrap_or_else(|| panic!("{} hat keinen [package]-Abschnitt", path.display()));
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{} hat kein [package] name", path.display()))
            .to_owned();
        let version_decl = parse_version_decl(package);
        all_members.push(MemberManifest { name, path, version_decl });
    }

    // Collect workspace-owned packages: name is "harw" or starts with "harw-".
    let workspace_packages: Vec<&MemberManifest> = all_members
        .iter()
        .filter(|m| m.name == "harw" || m.name.starts_with("harw-"))
        .collect();

    assert!(
        !workspace_packages.is_empty(),
        "No harw workspace packages found — workspace membership lookup failed"
    );

    // Resolve each member's effective version (own literal, or the
    // workspace-inherited one read above).
    let resolved_version = |m: &MemberManifest| -> Option<String> {
        match &m.version_decl {
            VersionDecl::Literal(v) => Some(v.clone()),
            VersionDecl::WorkspaceInherited => Some(root_workspace_version.clone()),
            VersionDecl::Missing => None,
        }
    };

    // ── Assertion 1: every workspace package is on EXPECTED_VERSION ───────
    let mut mismatched: Vec<String> = Vec::new();
    for pkg in &workspace_packages {
        match resolved_version(pkg) {
            Some(v) if v == EXPECTED_VERSION => {}
            Some(v) => mismatched.push(format!("{} = {}", pkg.name, v)),
            None => mismatched.push(format!("{} = <keine Versionsangabe>", pkg.name)),
        }
    }
    assert!(
        mismatched.is_empty(),
        "Packages NOT on {EXPECTED_VERSION}:\n{}",
        mismatched.join("\n")
    );

    // ── Assertion 2: no workspace package is on OLD_VERSION ───────────────
    let old_version_packages: Vec<String> = workspace_packages
        .iter()
        .filter(|m| resolved_version(m).as_deref() == Some(OLD_VERSION))
        .map(|m| m.name.clone())
        .collect();

    assert!(
        old_version_packages.is_empty(),
        "Packages still on {OLD_VERSION}: {:?}",
        old_version_packages
    );

    // ── Assertion 3: Cargo.lock resolves every internal package to
    // EXPECTED_VERSION ──────────────────────────────────────────────────
    // `Cargo.lock` ist selbst gültiges TOML: eine Folge von `[[package]]`-
    // Tabellen mit (mindestens) `name` und `version`.
    let lock_path = workspace_root.join("Cargo.lock");
    let lock_value = read_toml(&lock_path);
    let lock_packages = lock_value
        .get("package")
        .and_then(toml::Value::as_array)
        .expect("Cargo.lock hat keine [[package]]-Einträge");

    let mut lock_versions_by_name: HashMap<String, Vec<String>> = HashMap::new();
    for pkg in lock_packages {
        let name = pkg
            .get("name")
            .and_then(toml::Value::as_str)
            .expect("[[package]]-Eintrag in Cargo.lock ohne name")
            .to_owned();
        let version = pkg
            .get("version")
            .and_then(toml::Value::as_str)
            .expect("[[package]]-Eintrag in Cargo.lock ohne version")
            .to_owned();
        lock_versions_by_name.entry(name).or_default().push(version);
    }

    let lock_mismatches: Vec<String> = workspace_packages
        .iter()
        .filter_map(|pkg| {
            let versions = lock_versions_by_name.get(&pkg.name);
            let matches_expected = versions.is_some_and(|vs| vs.iter().any(|v| v == EXPECTED_VERSION));
            if matches_expected {
                None
            } else {
                Some(format!("{} locked at {:?}", pkg.name, versions))
            }
        })
        .collect();

    assert!(
        lock_mismatches.is_empty(),
        "Lockfile-resolved versions NOT at {EXPECTED_VERSION}:\n{}",
        lock_mismatches.join("\n")
    );

    // ── Assertion 4a: workspace root Cargo.toml contains EXPECTED_VERSION
    // exactly once ─────────────────────────────────────────────────────
    let root_toml_text =
        fs::read_to_string(&root_toml_path).unwrap_or_else(|e| panic!("Cannot read {}: {e}", root_toml_path.display()));

    let root_occurrences = count_occurrences(&root_toml_text, EXPECTED_VERSION);
    assert_eq!(
        root_occurrences, 1,
        "Workspace root Cargo.toml must contain '{EXPECTED_VERSION}' exactly once \
         (the [workspace.package] line), found {root_occurrences} occurrences"
    );

    // ── Assertion 4b: no member Cargo.toml pins its OWN version to
    // EXPECTED_VERSION literally ─────────────────────────────────────────
    // Anders als eine Volltextsuche verwechselt diese Prüfung — dank des
    // geparsten `[package] version`-Felds — "unsere Version" nicht mit einer
    // zufällig gleichlautenden Fremdversion (z. B. `sd-listen-fds = "0.2.0"`
    // in `harw-warden`s `[dependencies]`).
    let member_violations: Vec<String> = workspace_packages
        .iter()
        .filter(|m| matches!(&m.version_decl, VersionDecl::Literal(v) if v == EXPECTED_VERSION))
        .map(|m| format!("{} ({})", m.name, m.path.display()))
        .collect();
    assert!(
        member_violations.is_empty(),
        "Member Cargo.toml files must NOT contain '{EXPECTED_VERSION}' \
         (they inherit via version.workspace = true). Violations:\n{}",
        member_violations.join("\n")
    );

    // ── Assertion 5: no member Cargo.toml has a bare `version = "0.1.0"`
    // pin ─────────────────────────────────────────────────────────────
    let old_pin_violations: Vec<String> = workspace_packages
        .iter()
        .filter(|m| matches!(&m.version_decl, VersionDecl::Literal(v) if v == OLD_VERSION))
        .map(|m| format!("{} ({})", m.name, m.path.display()))
        .collect();
    assert!(
        old_pin_violations.is_empty(),
        "Member Cargo.toml files contain a bare `version = \"{OLD_VERSION}\"` pin. \
         Violations:\n{}",
        old_pin_violations.join("\n")
    );
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_count_occurrences_basic() {
        assert_eq!(count_occurrences("0.2.0 and 0.2.0 and 0.1.0", "0.2.0"), 2);
        assert_eq!(count_occurrences("nothing here", "0.2.0"), 0);
    }

    #[test]
    fn test_parse_version_decl_literal_string() {
        let manifest = toml::Value::Table(
            "[package]\nname = \"foo\"\nversion = \"0.2.0\"\n".parse::<toml::Table>().unwrap(),
        );
        let package = manifest.get("package").unwrap();
        assert_eq!(parse_version_decl(package), VersionDecl::Literal("0.2.0".to_owned()));
    }

    #[test]
    fn test_parse_version_decl_workspace_inherited() {
        let manifest = toml::Value::Table(
            "[package]\nname = \"foo\"\nversion.workspace = true\n".parse::<toml::Table>().unwrap(),
        );
        let package = manifest.get("package").unwrap();
        assert_eq!(parse_version_decl(package), VersionDecl::WorkspaceInherited);
    }

    #[test]
    fn test_parse_version_decl_missing_field() {
        let manifest = toml::Value::Table("[package]\nname = \"foo\"\n".parse::<toml::Table>().unwrap());
        let package = manifest.get("package").unwrap();
        assert_eq!(parse_version_decl(package), VersionDecl::Missing);
    }

    #[test]
    fn test_workspace_member_dirs_reads_literal_string_list() {
        let root = toml::Value::Table("[workspace]\nmembers = [\"a\", \"b\"]\n".parse::<toml::Table>().unwrap());
        let dirs = workspace_member_dirs(&root, Path::new("/tmp/ws"));
        assert_eq!(dirs, vec![PathBuf::from("/tmp/ws/a"), PathBuf::from("/tmp/ws/b")]);
    }
}
