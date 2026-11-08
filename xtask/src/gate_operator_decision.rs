//! Gate `operator-decision`: nur `harw-escalator` darf das Cargo-Feature
//! `operator-decision` von `harw-dod-rules` einschalten (PL-100 WP0.11,
//! Leitprinzip I1).
//!
//! # Auftrag
//! „Modelle beraten, ein Mensch autorisiert": `harw_dod_rules::triage_record`
//! deckelt jedes Modell-`Confirmed` auf `NeedsReview`. Der einzige produktive
//! Weg zu einem bestätigten Befund ist `triage_record_operator`, und den gibt
//! es nur mit dem Feature [`FEATURE`]. Wer das Feature einschaltet, kann
//! Befunde bestätigen und damit die Eskalationsleiter erreichen — das darf
//! ausschließlich der Escalator ([`ALLOWED_ENABLERS`]), ein eigener Prozess
//! ohne Modellzugang. Dieses Gate macht jede andere Crate, die das Feature
//! anfordert, rot.
//!
//! # Was als „einschalten" zählt
//! Gelesen werden die Manifeste selbst (`toml::Table`, wie das Gate `arch`),
//! nicht `cargo metadata` — kein Subprozess:
//!
//! - eine Abhängigkeit auf [`FEATURE_CRATE`] (auch umbenannt per
//!   `package = "harw-dod-rules"`, auch mit `workspace = true`) in
//!   `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]` oder einer
//!   `[target.'…'.*]`-Variante davon, deren `features` [`FEATURE`] nennt;
//! - ein Eintrag `"<abhängigkeit>/operator-decision"` bzw.
//!   `"<abhängigkeit>?/operator-decision"` in `[features]`;
//! - ein Eintrag in `[workspace.dependencies]` der Wurzel, dessen `features`
//!   [`FEATURE`] nennt — er gälte für jedes erbende Member;
//! - in [`FEATURE_CRATE`] selbst ein anderes Feature (auch `default`), das
//!   [`FEATURE`] mit einschaltet — sonst würde jede Crate, die z. B.
//!   `test-support` anfordert, es still mitbekommen.
//!
//! Dev-Abhängigkeiten zählen bewusst mit: das Feature hat auch in Tests
//! fremder Crates nichts verloren (dort gibt es `test-support`).
//!
//! # Was ein Gate schuldet
//! Fehlt [`FEATURE_CRATE`] im Graphen oder definiert es [`FEATURE`] nicht
//! (mehr), ist das ein Verstoß: sonst prüfte dieses Gate nach einer
//! Umbenennung still nichts (siehe `gates`-Moduldoku, G-102).
//!
//! # Nebenläufigkeit
//! Ein Lauf liest Dateien und hält keinen geteilten Zustand.
//!
//! # Fehler
//! [`run`] meldet unlesbare oder ungültige Manifeste als `Err(String)`;
//! Verstöße stehen im [`GateReport`].
//!
//! # Examples
//! ```text
//! cargo xtask gates operator-decision
//! ```

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use super::GateReport;

/// Die Crate, deren Feature dieses Gate bewacht.
pub const FEATURE_CRATE: &str = "harw-dod-rules";

/// Das bewachte Feature.
pub const FEATURE: &str = "operator-decision";

/// Die einzigen Crates, die [`FEATURE`] einschalten dürfen. `harw-escalator`
/// existiert noch nicht (PL-100 Phase 3); bis dahin darf niemand.
pub const ALLOWED_ENABLERS: [&str; 1] = ["harw-escalator"];

/// Die Abhängigkeitstabellen eines Manifests (inklusive der alten
/// Unterstrich-Schreibweisen, die Cargo weiterhin liest).
const DEPENDENCY_TABLES: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "dev_dependencies",
    "build_dependencies",
];

/// Ein eingelesenes Member-Manifest: Paketname und geparste Tabelle.
pub type MemberManifest = (String, toml::Table);

/// Führt das Gate im aktuellen Verzeichnis (der Repo-Wurzel) aus.
///
/// # Returns
/// Den [`GateReport`] über alle Member-Manifeste plus die Wurzel.
///
/// # Errors
/// Ein unlesbarer Workspace oder ein unlesbares bzw. ungültiges Manifest.
pub fn run() -> Result<GateReport, String> {
    let (root, members) = load_manifests(Path::new("."))?;
    Ok(evaluate(&root, &members))
}

/// Liest die Wurzel-`Cargo.toml` und jedes Member-Manifest unter
/// `repo_root`.
///
/// # Arguments
/// - `repo_root` (`&Path`): Wurzel des Repositorys.
///
/// # Returns
/// Die Wurzeltabelle und je Member `(Paketname, Tabelle)`.
///
/// # Errors
/// Ein unlesbarer Workspace oder ein unlesbares bzw. ungültiges Manifest.
pub fn load_manifests(repo_root: &Path) -> Result<(toml::Table, Vec<MemberManifest>), String> {
    let graph = super::load_all_workspaces(repo_root)?;
    let root = read_manifest(&repo_root.join("Cargo.toml"))?;
    let mut members = Vec::with_capacity(graph.crates.len());
    for node in &graph.crates {
        members.push((node.name.clone(), read_manifest(&node.manifest_path)?));
    }
    Ok((root, members))
}

/// Liest und parst ein Manifest.
fn read_manifest(path: &Path) -> Result<toml::Table, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// Prüft bereits geparste Manifeste.
///
/// # Description
/// Getrennt von [`run`], damit jeder rote Zweig gegen konstruierte
/// Manifeste prüfbar ist. Siehe Moduldoku für die Regeln.
///
/// # Arguments
/// - `root` (`&toml::Table`): die Wurzel-`Cargo.toml`.
/// - `members` (`&[MemberManifest]`): alle Member-Manifeste.
///
/// # Returns
/// Den [`GateReport`]; `checked` zählt die Member plus die Wurzel.
#[must_use]
pub fn evaluate(root: &toml::Table, members: &[MemberManifest]) -> GateReport {
    let mut violations = Vec::new();

    for location in workspace_enablements(root) {
        violations.push(format!(
            "Cargo.toml: {location} schaltet {FEATURE_CRATE}/{FEATURE} für jedes erbende Member \
             ein; nur {} darf das Feature anfordern (PL-100 WP0.11)",
            ALLOWED_ENABLERS.join(", ")
        ));
    }

    let mut owner_seen = false;
    for (name, manifest) in members {
        if name == FEATURE_CRATE {
            owner_seen = true;
            violations.extend(owner_violations(manifest));
            continue;
        }
        if ALLOWED_ENABLERS.contains(&name.as_str()) {
            continue;
        }
        for location in member_enablements(manifest) {
            violations.push(format!(
                "{name}: {location} schaltet {FEATURE_CRATE}/{FEATURE} ein; nur {} darf das \
                 Feature anfordern (PL-100 WP0.11, Leitprinzip I1)",
                ALLOWED_ENABLERS.join(", ")
            ));
        }
    }

    if !owner_seen {
        violations.push(format!(
            "{FEATURE_CRATE} fehlt im Workspace-Graphen; das Gate prüft sonst nichts"
        ));
    }

    GateReport {
        name: "operator-decision",
        checked: members.len().saturating_add(1),
        violations,
    }
}

/// Ob `value` (ein Abhängigkeitseintrag) [`FEATURE`] in `features` nennt.
fn requests_feature(value: &toml::Value) -> bool {
    value
        .as_table()
        .and_then(|table| table.get("features"))
        .and_then(toml::Value::as_array)
        .is_some_and(|features| {
            features
                .iter()
                .any(|feature| feature.as_str() == Some(FEATURE))
        })
}

/// Der Paketname eines Abhängigkeitseintrags: `package = "…"` oder der
/// Schlüssel selbst.
fn package_of<'a>(key: &'a str, value: &'a toml::Value) -> &'a str {
    value
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_str)
        .unwrap_or(key)
}

/// Fundstellen in `[workspace.dependencies]` der Wurzel.
fn workspace_enablements(root: &toml::Table) -> Vec<String> {
    let Some(dependencies) = root
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
    else {
        return Vec::new();
    };
    dependencies
        .iter()
        .filter(|(key, value)| package_of(key, value) == FEATURE_CRATE && requests_feature(value))
        .map(|(key, _)| format!("[workspace.dependencies].{key}"))
        .collect()
}

/// Alle Abhängigkeitstabellen eines Manifests mit ihrem Ort, auch unter
/// `[target.'…']`.
fn dependency_tables(manifest: &toml::Table) -> Vec<(String, &toml::Table)> {
    let mut tables = Vec::new();
    for kind in DEPENDENCY_TABLES {
        if let Some(table) = manifest.get(kind).and_then(toml::Value::as_table) {
            tables.push((format!("[{kind}]"), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, section) in targets {
            let Some(section) = section.as_table() else {
                continue;
            };
            for kind in DEPENDENCY_TABLES {
                if let Some(table) = section.get(kind).and_then(toml::Value::as_table) {
                    tables.push((format!("[target.'{target}'.{kind}]"), table));
                }
            }
        }
    }
    tables
}

/// Fundstellen in einem Member-Manifest (Abhängigkeiten und `[features]`).
fn member_enablements(manifest: &toml::Table) -> Vec<String> {
    let mut locations = Vec::new();
    // Unter diesen Namen ist `harw-dod-rules` in diesem Manifest erreichbar
    // (Schlüssel bzw. Umbenennung); der Paketname selbst zählt immer.
    let mut aliases: BTreeSet<String> = BTreeSet::from([FEATURE_CRATE.to_owned()]);

    for (context, table) in dependency_tables(manifest) {
        for (key, value) in table {
            if package_of(key, value) != FEATURE_CRATE {
                continue;
            }
            aliases.insert(key.clone());
            if requests_feature(value) {
                locations.push(format!("{context}.{key}"));
            }
        }
    }

    if let Some(features) = manifest.get("features").and_then(toml::Value::as_table) {
        for (feature, enables) in features {
            let Some(enables) = enables.as_array() else {
                continue;
            };
            for entry in enables.iter().filter_map(toml::Value::as_str) {
                if enables_via_alias(entry, &aliases) {
                    locations.push(format!("[features].{feature} = \"{entry}\""));
                }
            }
        }
    }

    locations
}

/// Ob ein `[features]`-Eintrag `<alias>/operator-decision` oder
/// `<alias>?/operator-decision` für einen der `aliases` ist.
fn enables_via_alias(entry: &str, aliases: &BTreeSet<String>) -> bool {
    let Some((dependency, feature)) = entry.split_once('/') else {
        return false;
    };
    let dependency = dependency.strip_suffix('?').unwrap_or(dependency);
    feature == FEATURE && aliases.contains(dependency)
}

/// Verstöße in [`FEATURE_CRATE`] selbst: das Feature muss existieren, und
/// kein anderes Feature (auch nicht `default`) darf es mit einschalten.
fn owner_violations(manifest: &toml::Table) -> Vec<String> {
    let Some(features) = manifest.get("features").and_then(toml::Value::as_table) else {
        return vec![format!(
            "{FEATURE_CRATE}: [features] fehlt, das Feature '{FEATURE}' ist nicht definiert; \
             das Gate prüft sonst nichts"
        )];
    };
    let mut violations = Vec::new();
    if !features.contains_key(FEATURE) {
        violations.push(format!(
            "{FEATURE_CRATE}: das Feature '{FEATURE}' ist nicht definiert; das Gate prüft sonst \
             nichts"
        ));
    }
    for (feature, enables) in features {
        if feature == FEATURE {
            continue;
        }
        let Some(enables) = enables.as_array() else {
            continue;
        };
        if enables.iter().any(|entry| entry.as_str() == Some(FEATURE)) {
            violations.push(format!(
                "{FEATURE_CRATE}: [features].{feature} schaltet '{FEATURE}' mit ein; jede Crate, \
                 die '{feature}' anfordert, bekäme es still mit"
            ));
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Wurzel des Repositorys (Cargo startet Tests im Paketverzeichnis).
    fn repo_root() -> TestResult<&'static Path> {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("Repo-Wurzel über xtask/"))
    }

    fn table(text: &str) -> TestResult<toml::Table> {
        toml::from_str(text).map_err(ctx("Test-Manifest parst"))
    }

    /// Ein regelkonformes `harw-dod-rules`-Manifest.
    fn owner() -> TestResult<MemberManifest> {
        Ok((
            FEATURE_CRATE.to_owned(),
            table("[features]\ntest-support = []\noperator-decision = []\n")?,
        ))
    }

    fn member(name: &str, text: &str) -> TestResult<MemberManifest> {
        Ok((name.to_owned(), table(text)?))
    }

    fn evaluate_members(extra: Vec<MemberManifest>) -> TestResult<GateReport> {
        let mut members = vec![owner()?];
        members.extend(extra);
        Ok(evaluate(&toml::Table::new(), &members))
    }

    /// Gegen den echten Workspace: niemand außer `harw-escalator` schaltet
    /// das Feature ein, und `harw-dod-rules` definiert es.
    #[test]
    fn test_real_workspace_is_green() -> TestResult {
        let (root, members) = load_manifests(repo_root()?).map_err(TestError::Unexpected)?;
        let report = evaluate(&root, &members);
        assert!(report.is_green(), "{:?}", report.violations);
        assert!(report.checked > 1, "{report:?}");
        Ok(())
    }

    #[test]
    fn test_plain_dependency_without_the_feature_is_green() -> TestResult {
        let report = evaluate_members(vec![member(
            "harw-dod-escalate",
            "[dependencies]\nharw-dod-rules = { path = \"../harw-dod-rules\" }\n\
             [dev-dependencies]\nharw-dod-rules = { path = \"../harw-dod-rules\", features = [\"test-support\"] }\n",
        )?])?;
        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    #[test]
    fn test_the_escalator_may_enable_the_feature() -> TestResult {
        let report = evaluate_members(vec![member(
            "harw-escalator",
            "[dependencies]\nharw-dod-rules = { path = \"x\", features = [\"operator-decision\"] }\n",
        )?])?;
        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    #[test]
    fn test_any_other_crate_enabling_the_feature_is_a_violation() -> TestResult {
        let report = evaluate_members(vec![
            member(
                "harw-runtime",
                "[dependencies]\nharw-dod-rules = { path = \"x\", features = [\"operator-decision\"] }\n",
            )?,
            member(
                "harw-dod-escalate",
                "[dev-dependencies]\nharw-dod-rules = { path = \"x\", features = [\"operator-decision\"] }\n",
            )?,
            member(
                "harw-ops",
                "[target.'cfg(unix)'.dependencies]\nharw-dod-rules = { workspace = true, features = [\"operator-decision\"] }\n",
            )?,
        ])?;
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.starts_with("harw-runtime: [dependencies]"))
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.starts_with("harw-dod-escalate: [dev-dependencies]"))
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.starts_with("harw-ops: [target.'cfg(unix)'.dependencies]"))
        );
        Ok(())
    }

    #[test]
    fn test_renamed_dependency_and_feature_entries_are_violations() -> TestResult {
        let report = evaluate_members(vec![
            member(
                "harw-cli",
                "[dependencies]\nrules = { package = \"harw-dod-rules\", path = \"x\", features = [\"operator-decision\"] }\n",
            )?,
            member(
                "harw-tui",
                "[dependencies]\nrules = { package = \"harw-dod-rules\", path = \"x\", optional = true }\n\
                 [features]\nconfirm = [\"rules?/operator-decision\"]\n\
                 direct = [\"harw-dod-rules/operator-decision\"]\n\
                 harmless = [\"rules/test-support\", \"dep:rules\"]\n",
            )?,
        ])?;
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.starts_with("harw-cli: [dependencies].rules"))
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.starts_with("harw-tui: [features].confirm"))
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.starts_with("harw-tui: [features].direct"))
        );
        Ok(())
    }

    #[test]
    fn test_workspace_dependency_enabling_the_feature_is_a_violation() -> TestResult {
        let root = table(
            "[workspace.dependencies]\nharw-dod-rules = { path = \"x\", features = [\"operator-decision\"] }\n",
        )?;
        let report = evaluate(&root, &[owner()?]);
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert!(
            report
                .violations
                .iter()
                .all(|v| v.contains("[workspace.dependencies]"))
        );
        Ok(())
    }

    #[test]
    fn test_owner_must_define_the_feature_and_never_imply_it() -> TestResult {
        let missing = evaluate(
            &toml::Table::new(),
            &[member(FEATURE_CRATE, "[features]\ntest-support = []\n")?],
        );
        assert_eq!(missing.violations.len(), 1, "{:?}", missing.violations);
        assert!(
            missing
                .violations
                .iter()
                .all(|v| v.contains("nicht definiert"))
        );

        let implied = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[features]\ndefault = [\"operator-decision\"]\n\
                 test-support = [\"operator-decision\"]\noperator-decision = []\n",
            )?],
        );
        assert_eq!(implied.violations.len(), 2, "{:?}", implied.violations);
        assert!(implied.violations.iter().all(|v| v.contains("mit ein")));
        Ok(())
    }

    #[test]
    fn test_missing_owner_is_not_green() {
        let report = evaluate(&toml::Table::new(), &[]);
        assert!(!report.is_green());
        assert!(report.violations.iter().any(|v| v.contains("fehlt")));
    }
}
