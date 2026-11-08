//! Gate `operator-decision`: nur `harw-escalator` darf das Cargo-Feature
//! `operator-decision` von `harw-dod-rules` einschalten; bevor es den
//! Escalator gibt, darf `harw-dod-rules` das Feature nicht einmal deklarieren;
//! und das Testhilfe-Feature `test-support` derselben Crate darf nur aus
//! Dev-Abhängigkeiten kommen (PL-100 WP0.11, Leitprinzip I1).
//!
//! # Auftrag
//! „Modelle beraten, ein Mensch autorisiert": `harw_dod_rules::triage_record`
//! deckelt jedes Modell-`Confirmed` auf `NeedsReview`. Der Weg eines Menschen
//! zu einem bestätigten Befund (`OperatorDecision`, `triage_record_operator`)
//! existiert seit dem Owner-Review von PR #168 nur unter `cfg(test)`; das
//! Feature [`FEATURE`] kehrt erst zusammen mit `harw-escalator` zurück
//! (PL-100 WP 3.2). Wer das Feature dann einschaltet, kann Befunde bestätigen
//! und damit die Eskalationsleiter erreichen — das darf ausschließlich der
//! Escalator ([`ALLOWED_ENABLERS`]), ein eigener Prozess ohne Modellzugang.
//!
//! Die Testhilfen hinter [`TEST_SUPPORT_FEATURE`] (`triage`,
//! `triaged_finding_for_test`) erzeugen einen `Finding<Triaged>` mit frei
//! wählbarem Urteil, also auch mit `Verdict::Confirmed`. Im produktiven Build
//! darf es sie deshalb ebenso wenig geben: nur Dev-Abhängigkeiten dürfen das
//! Feature anfordern (Regel (d)), dann ist es wegen `resolver = "2"` nie im
//! Build eines produktiven Konsumenten aktiv.
//!
//! # Paketidentität statt Tabellenschlüssel
//! Owner-Review PR #168, Befund 2: Ein Member-Eintrag
//! `rules = { workspace = true, features = ["operator-decision"] }` trägt
//! lokal kein `package`; erst der gleichnamige Wurzeleintrag
//! `rules = { package = "harw-dod-rules", … }` verrät das Paket. Die frühere
//! Fassung verglich nur den lokalen Schlüssel und ließ den Eintrag durch.
//! Jetzt wird jeder Abhängigkeitseintrag zuerst aufgelöst
//! (`resolve_dependency`, `names_package`):
//!
//! - ohne `workspace = true`: Paket = `package = "…"` oder der Schlüssel;
//! - mit `workspace = true`: zusätzlich der gleichnamige Eintrag aus
//!   `[workspace.dependencies]` der Wurzel mit **dessen** `package` bzw.
//!   Schlüssel; dessen `features` gelten additiv mit (so vererbt Cargo).
//!   Ein lokales `package` neben `workspace = true` wertet Cargo nicht aus;
//!   das Gate zählt trotzdem beide Lesarten (konservativ, ein Fehlalarm ist
//!   hier billiger als ein Durchschlupf).
//!
//! Gelesen werden die Manifeste selbst (`toml::Table`, wie das Gate `arch`)
//! und `Cargo.lock`, nicht `cargo metadata` — kein Subprozess, und jeder rote
//! Zweig ist gegen konstruierte Manifeste testbar.
//!
//! # Eingabemenge
//! Geprüft wird jedes lokale Paket, das Cargo baut, nicht nur die in
//! `[workspace].members` aufgezählten ([`load_manifests`]):
//!
//! - die Wurzel-`Cargo.toml` und jedes Member aus `members`;
//! - transitiv jedes Paket, das ein gelesenes Manifest über einen
//!   `path`-Eintrag erreicht — in jeder Abhängigkeitstabelle (auch Dev-,
//!   Build- und `[target.'…']`-Tabellen), in der Wurzel zusätzlich aus
//!   `[workspace.dependencies]`, `[patch.*]` und `[replace]`. Liegt ein
//!   solches Paket im Workspace-Verzeichnis, macht Cargo es implizit zum
//!   Member; liegt es außerhalb, wird es trotzdem gebaut. Jedes Manifest wird
//!   über seinen kanonischen Pfad genau einmal gelesen; ein fehlendes Ziel
//!   ist ein Fehler, wie für Cargo.
//!
//! **(g)** Als Gegenprobe muss jedes `[[package]]` in `Cargo.lock` ohne
//! `source` (also jedes lokale Paket, auch eines aus einem `[patch]` in
//! `.cargo/config.toml`) unter den gelesenen Paketen stehen
//! ([`lock_coverage`]); sonst ist es ein Verstoß, denn es könnte jede Regel
//! unbemerkt umgehen.
//!
//! # Verstöße
//! **(a)** Ein Paket außer `harw-escalator` fordert [`FEATURE`] an, auf
//! irgendeinem Weg:
//!
//! - eine (aufgelöste) Abhängigkeit auf [`FEATURE_CRATE`] in
//!   `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, deren
//!   Unterstrich-Schreibweisen oder einer `[target.'…'.*]`-Variante, deren
//!   eigene oder geerbte `features` [`FEATURE`] nennen — direkt, umbenannt,
//!   geerbt oder umbenannt-geerbt;
//! - ein `features`-Eintrag `"<x>/operator-decision"` bzw.
//!   `"<x>?/operator-decision"` an **irgendeiner** Abhängigkeit (Weiterleitung
//!   durch eine Zwischen-Crate; `<x>` ist ein Name in deren Manifest und hier
//!   nicht auflösbar, deshalb ohne Ansehen des Namens);
//! - ein Eintrag `"<alias>/operator-decision"` bzw.
//!   `"<alias>?/operator-decision"` in `[features]`, wobei `<alias>` jeder
//!   Schlüssel dieses Manifests ist, der auf [`FEATURE_CRATE`] auflöst (auch
//!   ein geerbter, umbenannter);
//! - ein Eintrag in `[workspace.dependencies]` der Wurzel, der [`FEATURE`]
//!   anfordert — er gälte für jedes erbende Member; auch der Escalator fordert
//!   das Feature in seinem eigenen Manifest an, nicht über die Wurzel.
//!
//! Auch der Escalator fordert [`FEATURE`] nur in einem Abhängigkeitseintrag
//! an, nie über seine eigenen `[features]`: Eine Weiterleitung
//! `confirm = ["rules?/operator-decision"]` gäbe das Feature jeder Crate, die
//! `harw-escalator/confirm` einschaltet, ohne dass ihr Manifest
//! `/operator-decision` nennt.
//!
//! Eine Wurzel mit `[package]` ist selbst ein Paket und wird wie ein Member
//! geprüft; [`FEATURE_CRATE`] selbst ebenso (Selbst-Dev-Abhängigkeit).
//! Dev-Abhängigkeiten zählen bewusst mit: das Feature hat auch in Tests
//! fremder Crates nichts verloren (dort gibt es `test-support`).
//!
//! **(b)** [`FEATURE_CRATE`] deklariert [`FEATURE`] — in `[features]` oder
//! implizit als optionale Abhängigkeit dieses Namens —, solange kein
//! Paket `harw-escalator` existiert. Ohne Escalator gibt es keinen erlaubten
//! Konsumenten; ein deklariertes Feature wäre nur eine Einladung.
//!
//! **(c)** In [`FEATURE_CRATE`] schaltet ein anderes Feature (auch `default`)
//! [`FEATURE`] mit ein — sonst bekäme jede Crate, die z. B. `test-support`
//! anfordert, es still mit. Gezählt werden `"operator-decision"`,
//! `"dep:operator-decision"` sowie `"operator-decision/…"` und
//! `"operator-decision?/…"` (das implizite Feature einer gleichnamigen
//! optionalen Abhängigkeit; `?` und `dep:` konservativ mitgezählt). Regel (c)
//! gilt auch, wenn der Escalator existiert.
//!
//! **(d)** [`TEST_SUPPORT_FEATURE`] von [`FEATURE_CRATE`] kommt aus einer
//! anderen als einer Dev-Abhängigkeit — für **jedes** Paket, auch den
//! Escalator:
//!
//! - eine (aufgelöste) Abhängigkeit auf [`FEATURE_CRATE`] in einer Tabelle
//!   außer `[dev-dependencies]`/`[dev_dependencies]` (auch unter
//!   `[target.'…']`), deren eigene oder geerbte `features`
//!   [`TEST_SUPPORT_FEATURE`] nennen — direkt, umbenannt, geerbt oder
//!   umbenannt-geerbt;
//! - ein `features`-Eintrag `"<x>/test-support"` bzw. `"<x>?/test-support"`
//!   an einer solchen Nicht-Dev-Abhängigkeit (ohne Ansehen von `<x>`, wie bei
//!   (a));
//! - ein `[features]`-Eintrag `"<alias>/test-support"` bzw.
//!   `"<alias>?/test-support"` für einen Alias von [`FEATURE_CRATE`] — auch
//!   dann, wenn der Alias nur als Dev-Abhängigkeit vorkommt (derselbe
//!   Schlüssel kann zugleich in `[dependencies]` stehen; konservativ);
//! - ein Eintrag in `[workspace.dependencies]`, der [`TEST_SUPPORT_FEATURE`]
//!   anfordert — er gälte auch für erbende `[dependencies]`;
//! - in [`FEATURE_CRATE`] ein anderes Feature (auch `default`), das
//!   [`TEST_SUPPORT_FEATURE`] mit einschaltet (gezählt wie bei (c)).
//!
//! **(e)** [`FEATURE_CRATE`] deklariert ein Feature außerhalb der Allowlist
//! [`OWNER_FEATURES`] (`default`, `test-support`; dazu [`FEATURE`], sobald der
//! Escalator existiert, siehe (b)) — explizit in `[features]` oder implizit
//! über eine optionale Abhängigkeit, die kein `"dep:<name>"` unterdrückt.
//! Die Regeln (b)–(d) bewachen nur die Namen [`FEATURE`] und
//! [`TEST_SUPPORT_FEATURE`]; ohne Allowlist ließe sich der Operator-Pfad unter
//! einem frei gewählten Namen (`human = []` plus
//! `cfg(any(test, feature = "human"))`) an Gate und `compile_fail`-Doctests
//! vorbei freischalten.
//!
//! **(f)** Ein anderes Paket hängt (aufgelöst, in irgendeiner Tabelle) von
//! `harw-escalator` ab. Der Escalator ist ein eigener Prozess; wer ihn als
//! Bibliothek einbindet, teilt sich sein `harw-dod-rules` samt [`FEATURE`]
//! und könnte seine `[features]` einschalten.
//!
//! **(g)** siehe „Eingabemenge".
//!
//! # Grenzen
//! - Cargo vereinheitlicht Features je Build: Sobald `harw-escalator` das
//!   Feature einschaltet, sieht in einem gemeinsamen Build (etwa
//!   `cargo build --workspace`) jede andere Crate dasselbe `harw-dod-rules`
//!   mit Feature. Ein Manifest-Gate verhindert das Anfordern, nicht das
//!   Mitbenutzen; WP 3.2 braucht deshalb zusätzlich eine nicht fälschbare,
//!   Escalator-eigene Berechtigung (siehe PL-100-README, WP 3.2).
//! - Dasselbe gilt für Regel (d): Ein Lauf, der Dev-Ziele baut (`cargo test`,
//!   `--all-targets`) oder Features pauschal einschaltet (`--all-features`,
//!   `--features harw-dod-rules/test-support`), aktiviert `test-support` für
//!   alle Ziele dieses Laufs, auch für Binaries. Die Prüfläufe des Makefile
//!   (`check`, `clippy`, `tests`, `clippy-tests`) nutzen
//!   `--workspace --all-features`; ihre Artefakte sind keine
//!   Release-Artefakte. Release-Artefakte entstehen je Paket mit `-p` und
//!   ohne `--all-features`/`--features` (`.github/workflows/release.yml:101-108`,
//!   `Makefile:126-127`). Das ist eine Anforderung an die Release-Pipeline;
//!   das Gate sichert die Manifeste, nicht die Build-Kommandos.
//! - Kommandozeilen-Features (`--features …`) sind kein Manifest. Solange
//!   Regel (b) gilt, existiert [`FEATURE`] nicht, und Cargo lehnt
//!   `--features harw-dod-rules/operator-decision` ab.
//! - `workspace = true` in einem Paket außerhalb des Workspace-Verzeichnisses
//!   wird gegen **diese** Wurzel aufgelöst; die eigene Workspace-Wurzel eines
//!   solchen Pakets liest das Gate nicht.
//! - Dass `OperatorDecision` und `triage_record_operator` außerhalb von
//!   `cfg(test)` nicht existieren, belegen `compile_fail`-Doctests an
//!   `harw_dod_rules::TriageError`, nicht dieses Gate.
//!
//! # Was ein Gate schuldet
//! Fehlt [`FEATURE_CRATE`] im Graphen, ist das ein Verstoß: sonst prüfte
//! dieses Gate nach einer Umbenennung still nichts (siehe `gates`-Moduldoku,
//! G-102). Ein **fehlendes** Feature ist dagegen bis WP 3.2 der Sollzustand.
//!
//! # Nebenläufigkeit
//! Ein Lauf liest Dateien und hält keinen geteilten Zustand.
//!
//! # Fehler
//! [`run`] meldet unlesbare oder ungültige Manifeste, ein fehlendes Ziel
//! einer `path`-Abhängigkeit und ein unlesbares `Cargo.lock` als
//! `Err(String)`; Verstöße stehen im [`GateReport`].
//!
//! # Examples
//! ```text
//! cargo xtask gates operator-decision
//! ```

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::GateReport;

/// Die Crate, deren Feature dieses Gate bewacht.
pub const FEATURE_CRATE: &str = "harw-dod-rules";

/// Das bewachte Feature.
pub const FEATURE: &str = "operator-decision";

/// Das Testhilfe-Feature von [`FEATURE_CRATE`] (`triage`,
/// `triaged_finding_for_test`): erzeugt einen `Finding<Triaged>` mit frei
/// wählbarem Urteil, also auch mit `Verdict::Confirmed`, und ist deshalb nur
/// aus Dev-Abhängigkeiten erlaubt (Regel (d) der Moduldoku).
pub const TEST_SUPPORT_FEATURE: &str = "test-support";

/// Die einzigen Crates, die [`FEATURE`] einschalten dürfen. `harw-escalator`
/// existiert noch nicht (PL-100 WP 3.2); bis dahin darf niemand das Feature
/// anfordern, und [`FEATURE_CRATE`] darf es nicht deklarieren (Regel (b) der
/// Moduldoku). Kein anderes Paket darf von ihnen abhängen (Regel (f)).
pub const ALLOWED_ENABLERS: [&str; 1] = ["harw-escalator"];

/// Die Features, die [`FEATURE_CRATE`] immer deklarieren darf (Regel (e) der
/// Moduldoku); [`FEATURE`] kommt hinzu, sobald der Escalator existiert
/// (Regel (b)). Was ein erlaubtes Feature mit einschaltet, begrenzen die
/// Regeln (c) und (d).
pub const OWNER_FEATURES: [&str; 2] = ["default", TEST_SUPPORT_FEATURE];

/// Die Abhängigkeitstabellen eines Manifests (inklusive der alten
/// Unterstrich-Schreibweisen, die Cargo weiterhin liest).
const DEPENDENCY_TABLES: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "dev_dependencies",
    "build_dependencies",
];

/// Die Abhängigkeitstabellen aus [`DEPENDENCY_TABLES`], die nur
/// Dev-Abhängigkeiten tragen (Regel (d)).
const DEV_DEPENDENCY_TABLES: [&str; 2] = ["dev-dependencies", "dev_dependencies"];

/// Ein eingelesenes Paket-Manifest: Paketname und geparste Tabelle.
pub type MemberManifest = (String, toml::Table);

/// Führt das Gate im aktuellen Verzeichnis (der Repo-Wurzel) aus.
///
/// # Returns
/// Den [`GateReport`] über alle lokalen Pakete plus die Wurzel.
///
/// # Errors
/// Wie [`run_at`].
pub fn run() -> Result<GateReport, String> {
    run_at(Path::new("."))
}

/// Führt das Gate gegen den Workspace unter `repo_root` aus.
///
/// # Description
/// Liest Wurzel und Pakete ([`load_manifests`]), prüft sie ([`evaluate`])
/// und belegt die Vollständigkeit der Eingabemenge gegen
/// `<repo_root>/Cargo.lock` ([`lock_coverage`], Regel (g)).
///
/// # Arguments
/// - `repo_root` (`&Path`): Wurzel des Repositorys.
///
/// # Returns
/// Den [`GateReport`]; Verstöße aus [`lock_coverage`] stehen hinter denen
/// aus [`evaluate`].
///
/// # Errors
/// Ein unlesbarer Workspace, ein unlesbares bzw. ungültiges Manifest, ein
/// fehlendes Ziel einer `path`-Abhängigkeit oder ein unlesbares bzw.
/// ungültiges `Cargo.lock`.
pub fn run_at(repo_root: &Path) -> Result<GateReport, String> {
    let (root, packages) = load_manifests(repo_root)?;
    let lock_path = repo_root.join("Cargo.lock");
    let lock = fs::read_to_string(&lock_path)
        .map_err(|error| format!("{}: {error}", lock_path.display()))?;
    let mut report = evaluate(&root, &packages);
    report
        .violations
        .extend(lock_coverage(&lock, &root, &packages)?);
    Ok(report)
}

/// Liest die Wurzel-`Cargo.toml`, jedes Member-Manifest und transitiv jedes
/// über einen `path`-Eintrag erreichbare Paket unter `repo_root`.
///
/// # Description
/// Siehe Moduldoku, „Eingabemenge". Die Member kommen in der Reihenfolge
/// des Workspace-Graphen zuerst, danach die nur über `path` erreichbaren
/// Pakete. Jedes Manifest wird über seinen kanonischen Pfad genau einmal
/// gelesen; die Wurzel steht nie unter den Paketen (eine Wurzel mit
/// `[package]` prüft [`evaluate`] gesondert).
///
/// # Arguments
/// - `repo_root` (`&Path`): Wurzel des Repositorys.
///
/// # Returns
/// Die Wurzeltabelle und je Paket `(Paketname, Tabelle)`.
///
/// # Errors
/// Ein unlesbarer Workspace, ein unlesbares bzw. ungültiges Manifest, ein
/// fehlendes Ziel einer `path`-Abhängigkeit oder ein Ziel ohne
/// `[package].name`.
pub fn load_manifests(repo_root: &Path) -> Result<(toml::Table, Vec<MemberManifest>), String> {
    let graph = super::load_all_workspaces(repo_root)?;
    let root_path = repo_root.join("Cargo.toml");
    let root = read_manifest(&root_path)?;
    // Bereits gelesene Manifeste, kanonisch. Die Wurzel ist kein Eintrag in
    // `packages`, darf aber auch nicht über einen `path`-Eintrag erneut
    // hineingeraten.
    let mut seen: BTreeSet<PathBuf> = BTreeSet::from([canonical(&root_path)?]);
    let mut pending = path_dependency_dirs(repo_root, &root, ManifestRole::Root);
    let mut packages = Vec::with_capacity(graph.crates.len());
    for node in &graph.crates {
        let manifest = read_manifest(&node.manifest_path)?;
        seen.insert(canonical(&node.manifest_path)?);
        pending.extend(path_dependency_dirs(
            &node.dir,
            &manifest,
            ManifestRole::Package,
        ));
        packages.push((node.name.clone(), manifest));
    }
    // Pakete außerhalb von `members`: im Workspace-Verzeichnis macht Cargo sie
    // implizit zu Membern, außerhalb baut es sie trotzdem.
    while let Some(dir) = pending.pop() {
        let manifest_path = dir.join("Cargo.toml");
        if !seen.insert(canonical(&manifest_path)?) {
            continue;
        }
        let manifest = read_manifest(&manifest_path)?;
        let name = package_name(&manifest)
            .ok_or_else(|| {
                format!(
                    "{}: [package].name fehlt (Ziel einer path-Abhängigkeit)",
                    manifest_path.display()
                )
            })?
            .to_owned();
        pending.extend(path_dependency_dirs(&dir, &manifest, ManifestRole::Package));
        packages.push((name, manifest));
    }
    Ok((root, packages))
}

/// Liest und parst ein Manifest.
fn read_manifest(path: &Path) -> Result<toml::Table, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// Der kanonische Pfad eines Manifests (Schlüssel gegen doppeltes Lesen);
/// ein fehlendes Manifest ist ein Fehler.
fn canonical(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Woher ein Manifest stammt: nur in der Wurzel wirken
/// `[workspace.dependencies]`, `[patch.*]` und `[replace]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestRole {
    /// Die Wurzel-`Cargo.toml`.
    Root,
    /// Ein Paket-Manifest (Member oder über `path` erreicht).
    Package,
}

/// Die Verzeichnisse aller `path`-Einträge eines Manifests, relativ zu
/// `base` (dem Verzeichnis des Manifests) aufgelöst.
///
/// # Description
/// Gelesen werden alle Abhängigkeitstabellen (auch unter `[target.'…']`),
/// für die Wurzel zusätzlich `[workspace.dependencies]`, jede Tabelle unter
/// `[patch]` und `[replace]`.
fn path_dependency_dirs(base: &Path, manifest: &toml::Table, role: ManifestRole) -> Vec<PathBuf> {
    let mut tables: Vec<&toml::Table> = dependency_tables(manifest)
        .into_iter()
        .map(|dependencies| dependencies.table)
        .collect();
    if role == ManifestRole::Root {
        tables.extend(workspace_dependencies(manifest));
        if let Some(patches) = manifest.get("patch").and_then(toml::Value::as_table) {
            tables.extend(patches.values().filter_map(toml::Value::as_table));
        }
        tables.extend(manifest.get("replace").and_then(toml::Value::as_table));
    }
    tables
        .into_iter()
        .flat_map(|table| table.values())
        .filter_map(|entry| entry.as_table()?.get("path")?.as_str())
        .map(|path| base.join(path))
        .collect()
}

/// Belegt die Vollständigkeit der Eingabemenge gegen `Cargo.lock`
/// (Regel (g) der Moduldoku).
///
/// # Description
/// Jedes `[[package]]` ohne `source` ist ein lokales Paket, das Cargo baut
/// (Member, `path`-Abhängigkeit, lokaler `[patch]`, auch aus
/// `.cargo/config.toml`). Steht sein Name weder unter `packages` noch ist er
/// die Wurzel, hat das Gate es nicht geprüft.
///
/// # Arguments
/// - `lock` (`&str`): der Inhalt von `Cargo.lock`.
/// - `root` (`&toml::Table`): die Wurzel-`Cargo.toml`.
/// - `packages` (`&[MemberManifest]`): die gelesenen Pakete.
///
/// # Returns
/// Je nicht gelesenem lokalem Paket ein Verstoß, nach Name sortiert.
///
/// # Errors
/// Ein ungültiges `Cargo.lock` oder ein lokales `[[package]]` ohne `name`.
pub fn lock_coverage(
    lock: &str,
    root: &toml::Table,
    packages: &[MemberManifest],
) -> Result<Vec<String>, String> {
    let lock: toml::Table = toml::from_str(lock).map_err(|error| format!("Cargo.lock: {error}"))?;
    let read: BTreeSet<&str> = packages
        .iter()
        .map(|(name, _)| name.as_str())
        .chain(package_name(root))
        .collect();
    let mut unread = BTreeSet::new();
    for entry in lock
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        if entry.get("source").is_some() {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| "Cargo.lock: lokales [[package]] ohne 'name'".to_owned())?;
        if !read.contains(name) {
            unread.insert(name);
        }
    }
    Ok(unread
        .into_iter()
        .map(|name| {
            format!(
                "{name}: lokales Paket laut Cargo.lock (ohne source), das das Gate nicht gelesen \
                 hat — weder Member noch über einen path-Eintrag erreichbar (etwa ein [patch] aus \
                 .cargo/config.toml); es könnte {FEATURE_CRATE}/{FEATURE} oder \
                 {FEATURE_CRATE}/{TEST_SUPPORT_FEATURE} unbemerkt anfordern (PL-100 WP0.11, \
                 Regel (g))"
            )
        })
        .collect())
}

/// Prüft bereits geparste Manifeste.
///
/// # Description
/// Getrennt von [`run`], damit jeder rote Zweig gegen konstruierte
/// Manifeste prüfbar ist. Siehe Moduldoku für die Regeln (a)–(f) und die
/// Auflösung der Paketidentität; Regel (g) prüft [`lock_coverage`].
///
/// # Arguments
/// - `root` (`&toml::Table`): die Wurzel-`Cargo.toml`.
/// - `members` (`&[MemberManifest]`): alle Pakete (Member und über `path`
///   erreichte, siehe [`load_manifests`]).
///
/// # Returns
/// Den [`GateReport`]; `checked` zählt die Pakete plus die Wurzel.
#[must_use]
pub fn evaluate(root: &toml::Table, members: &[MemberManifest]) -> GateReport {
    let workspace = workspace_dependencies(root);
    let mut violations = Vec::new();

    for location in workspace_requests(workspace, FEATURE) {
        violations.push(format!(
            "Cargo.toml: {location} schaltet {FEATURE_CRATE}/{FEATURE} für jedes erbende Member \
             ein; nur {} darf das Feature anfordern, und zwar im eigenen Manifest (PL-100 WP0.11)",
            ALLOWED_ENABLERS.join(", ")
        ));
    }
    for location in workspace_requests(workspace, TEST_SUPPORT_FEATURE) {
        violations.push(format!(
            "Cargo.toml: {location} schaltet {FEATURE_CRATE}/{TEST_SUPPORT_FEATURE} für jedes \
             erbende Member ein, auch in [dependencies]; die Testhilfen erzeugen Befunde mit \
             beliebigem Urteil und gehören nur in die [dev-dependencies] der Member (PL-100 \
             WP0.11, Regel (d))"
        ));
    }

    let escalator_present = members
        .iter()
        .any(|(name, _)| ALLOWED_ENABLERS.contains(&name.as_str()));

    // Eine Wurzel mit `[package]` ist selbst ein Paket des Workspace (Cargo
    // zählt sie implizit zu den Membern), steht aber nicht im Graphen. Steht
    // sie doch unter den Membern, wird sie dort geprüft, nicht doppelt.
    let root_package = package_name(root)
        .filter(|root_name| !members.iter().any(|(name, _)| name.as_str() == *root_name))
        .map(|root_name| (root_name, root));
    let packages = members
        .iter()
        .map(|(name, manifest)| (name.as_str(), manifest))
        .chain(root_package);

    let mut owner_seen = false;
    for (name, manifest) in packages {
        let is_enabler = ALLOWED_ENABLERS.contains(&name);
        if name == FEATURE_CRATE {
            owner_seen = true;
            violations.extend(owner_violations(manifest, escalator_present));
        }
        // Regel (d) gilt für jedes Paket, auch für den Escalator.
        let fixtures =
            member_requests(manifest, workspace, TEST_SUPPORT_FEATURE, Scope::OutsideDev);
        for location in fixtures.dependencies.iter().chain(&fixtures.features) {
            violations.push(format!(
                "{name}: {location} schaltet {FEATURE_CRATE}/{TEST_SUPPORT_FEATURE} außerhalb \
                 von [dev-dependencies] ein; die Testhilfen (triage, triaged_finding_for_test) \
                 erzeugten im produktiven Build Befunde mit beliebigem Urteil, auch \
                 Verdict::Confirmed (PL-100 WP0.11, Regel (d))"
            ));
        }

        let requests = member_requests(manifest, workspace, FEATURE, Scope::AllTables);
        if is_enabler {
            // Regel (a), zweiter Absatz: der Escalator selbst nur im
            // Abhängigkeitseintrag, nie über seine `[features]`.
            for location in &requests.features {
                violations.push(format!(
                    "{name}: {location} reicht {FEATURE_CRATE}/{FEATURE} über ein eigenes Feature \
                     weiter; jede Crate, die dieses Feature von {name} einschaltet, bekäme es \
                     mit — der Escalator fordert es nur im Abhängigkeitseintrag auf \
                     {FEATURE_CRATE} an (PL-100 WP0.11, Regel (a))"
                ));
            }
            continue;
        }
        for location in requests.dependencies.iter().chain(&requests.features) {
            violations.push(format!(
                "{name}: {location} schaltet {FEATURE_CRATE}/{FEATURE} ein; nur {} darf das \
                 Feature anfordern (PL-100 WP0.11, Leitprinzip I1)",
                ALLOWED_ENABLERS.join(", ")
            ));
        }
        for (location, enabler) in enabler_dependencies(manifest, workspace) {
            violations.push(format!(
                "{name}: {location} hängt von {enabler} ab; der Escalator ist ein eigener \
                 Prozess, wer ihn einbindet, teilt sich {FEATURE_CRATE} samt {FEATURE} und \
                 könnte seine Features einschalten (PL-100 WP0.11, Regel (f))"
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

/// In welchen Abhängigkeitstabellen eine Anforderung ein Verstoß ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// In jeder Tabelle, auch in Dev-Abhängigkeiten ([`FEATURE`], Regel (a)).
    AllTables,
    /// Nur außerhalb von Dev-Abhängigkeiten ([`TEST_SUPPORT_FEATURE`],
    /// Regel (d)).
    OutsideDev,
}

/// Fundstellen einer Anforderung in einem Paket-Manifest, nach Herkunft
/// getrennt (der Escalator darf nur die erste Art, siehe Regel (a)).
#[derive(Debug, Default)]
struct Requests {
    /// Aus Abhängigkeitseinträgen: eigene oder geerbte `features` auf
    /// [`FEATURE_CRATE`] oder eine Weiterleitung `<x>/<feature>` über eine
    /// andere Abhängigkeit.
    dependencies: Vec<String>,
    /// Aus den `[features]` des Pakets: `<alias>/<feature>` bzw.
    /// `<alias>?/<feature>` für einen Alias von [`FEATURE_CRATE`].
    features: Vec<String>,
}

/// Ein Abhängigkeitseintrag nach Auflösung von Umbenennung und
/// Workspace-Vererbung, bezogen auf ein geprüftes Feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedDependency {
    /// Ob der Eintrag unter mindestens einer Lesart [`FEATURE_CRATE`]
    /// bezeichnet (lokales `package`, Schlüssel oder geerbter Wurzeleintrag).
    targets_feature_crate: bool,
    /// Ob die eigenen `features` des Eintrags das geprüfte Feature anfordern.
    requests_locally: bool,
    /// Ob der per `workspace = true` geerbte Wurzeleintrag das geprüfte
    /// Feature anfordert (Cargo vereinigt dessen `features` mit den lokalen).
    requests_by_inheritance: bool,
}

/// Löst einen Abhängigkeitseintrag `key = value` zu seiner Paketidentität
/// auf und prüft, ob er `feature` anfordert.
///
/// # Description
/// Ohne `workspace = true` zählt `package = "…"` bzw. der Schlüssel. Mit
/// `workspace = true` kommt der gleichnamige Eintrag aus
/// `[workspace.dependencies]` hinzu, mit seinem eigenen `package` und seinen
/// `features`. Beide Lesarten zählen (siehe Moduldoku).
///
/// # Arguments
/// - `key` (`&str`): der Schlüssel im Member-Manifest (ggf. ein Alias).
/// - `value` (`&toml::Value`): der Eintrag selbst.
/// - `workspace` (`Option<&toml::Table>`): `[workspace.dependencies]` der
///   Wurzel, falls vorhanden.
/// - `feature` (`&str`): das geprüfte Feature ([`FEATURE`] oder
///   [`TEST_SUPPORT_FEATURE`]).
///
/// # Returns
/// Die [`ResolvedDependency`].
fn resolve_dependency(
    key: &str,
    value: &toml::Value,
    workspace: Option<&toml::Table>,
    feature: &str,
) -> ResolvedDependency {
    let targets_feature_crate = names_package(key, value, workspace, FEATURE_CRATE);
    ResolvedDependency {
        targets_feature_crate,
        requests_locally: requests_feature(value, feature, targets_feature_crate),
        requests_by_inheritance: inherited_entry(key, value, workspace)
            .is_some_and(|root_entry| requests_feature(root_entry, feature, targets_feature_crate)),
    }
}

/// Ob ein Abhängigkeitseintrag `key = value` unter mindestens einer Lesart
/// das Paket `package` bezeichnet: lokales `package = "…"` bzw. der
/// Schlüssel, und mit `workspace = true` zusätzlich `package` bzw. Schlüssel
/// des gleichnamigen Wurzeleintrags.
fn names_package(
    key: &str,
    value: &toml::Value,
    workspace: Option<&toml::Table>,
    package: &str,
) -> bool {
    package_of(key, value) == package
        || inherited_entry(key, value, workspace)
            .is_some_and(|root_entry| package_of(key, root_entry) == package)
}

/// Der per `workspace = true` geerbte Eintrag aus `[workspace.dependencies]`,
/// falls `value` erbt und die Wurzel einen Eintrag dieses Schlüssels hat.
fn inherited_entry<'w>(
    key: &str,
    value: &toml::Value,
    workspace: Option<&'w toml::Table>,
) -> Option<&'w toml::Value> {
    if inherits_from_workspace(value) {
        workspace.and_then(|dependencies| dependencies.get(key))
    } else {
        None
    }
}

/// Ob ein Abhängigkeitseintrag per `workspace = true` aus der Wurzel erbt.
fn inherits_from_workspace(value: &toml::Value) -> bool {
    value
        .as_table()
        .and_then(|table| table.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true)
}

/// Ob ein Abhängigkeitseintrag `optional = true` trägt.
fn is_optional(value: &toml::Value) -> bool {
    value
        .as_table()
        .and_then(|table| table.get("optional"))
        .and_then(toml::Value::as_bool)
        == Some(true)
}

/// Der Paketname eines Abhängigkeitseintrags ohne Vererbung:
/// `package = "…"` oder der Schlüssel selbst.
fn package_of<'a>(key: &'a str, value: &'a toml::Value) -> &'a str {
    value
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_str)
        .unwrap_or(key)
}

/// Die Einträge der `features`-Liste eines Abhängigkeitseintrags.
fn dependency_features(value: &toml::Value) -> impl Iterator<Item = &str> {
    value
        .as_table()
        .and_then(|table| table.get("features"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
}

/// Ob die `features` eines Abhängigkeitseintrags `feature` anfordern.
fn requests_feature(value: &toml::Value, feature: &str, targets_feature_crate: bool) -> bool {
    dependency_features(value)
        .any(|entry| feature_entry_requests(entry, feature, targets_feature_crate))
}

/// Ob ein einzelner Eintrag der `features`-Liste einer Abhängigkeit
/// `feature` anfordert.
///
/// # Description
/// Direkt (`"<feature>"`) zählt nur, wenn die Abhängigkeit
/// [`FEATURE_CRATE`] ist; weitergeleitet (`"<x>/<feature>"`,
/// `"<x>?/<feature>"`) zählt bei jeder Abhängigkeit, weil `<x>` ein Name in
/// deren Manifest ist.
fn feature_entry_requests(entry: &str, feature: &str, targets_feature_crate: bool) -> bool {
    match entry.split_once('/') {
        Some((_, requested)) => requested == feature,
        None => targets_feature_crate && entry == feature,
    }
}

/// `[workspace.dependencies]` der Wurzel, falls vorhanden.
fn workspace_dependencies(root: &toml::Table) -> Option<&toml::Table> {
    root.get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
}

/// `[package].name` eines Manifests, falls es ein Paket beschreibt (für die
/// Wurzel: falls sie selbst ein Paket ist).
fn package_name(manifest: &toml::Table) -> Option<&str> {
    manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
}

/// Fundstellen in `[workspace.dependencies]` der Wurzel, die `feature`
/// anfordern.
fn workspace_requests(workspace: Option<&toml::Table>, feature: &str) -> Vec<String> {
    let Some(dependencies) = workspace else {
        return Vec::new();
    };
    dependencies
        .iter()
        .filter(|(key, value)| {
            requests_feature(value, feature, package_of(key, value) == FEATURE_CRATE)
        })
        .map(|(key, _)| format!("[workspace.dependencies].{key}"))
        .collect()
}

/// Eine Abhängigkeitstabelle eines Manifests mit ihrem Ort.
struct DependencyTable<'a> {
    /// Ort für die Meldung, z. B. `[dev-dependencies]` oder
    /// `[target.'cfg(unix)'.dependencies]`.
    context: String,
    /// Ob die Tabelle nur Dev-Abhängigkeiten trägt (auch unter
    /// `[target.'…']`).
    dev: bool,
    /// Die Einträge der Tabelle.
    table: &'a toml::Table,
}

/// Alle Abhängigkeitstabellen eines Manifests mit ihrem Ort, auch unter
/// `[target.'…']`.
fn dependency_tables(manifest: &toml::Table) -> Vec<DependencyTable<'_>> {
    let mut tables = Vec::new();
    for kind in DEPENDENCY_TABLES {
        if let Some(table) = manifest.get(kind).and_then(toml::Value::as_table) {
            tables.push(DependencyTable {
                context: format!("[{kind}]"),
                dev: DEV_DEPENDENCY_TABLES.contains(&kind),
                table,
            });
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, section) in targets {
            let Some(section) = section.as_table() else {
                continue;
            };
            for kind in DEPENDENCY_TABLES {
                if let Some(table) = section.get(kind).and_then(toml::Value::as_table) {
                    tables.push(DependencyTable {
                        context: format!("[target.'{target}'.{kind}]"),
                        dev: DEV_DEPENDENCY_TABLES.contains(&kind),
                        table,
                    });
                }
            }
        }
    }
    tables
}

/// Fundstellen in einem Paket-Manifest (Abhängigkeiten und `[features]`),
/// die `feature` von [`FEATURE_CRATE`] anfordern, nach Auflösung jedes
/// Eintrags über `workspace` (Regeln (a) und (d)).
///
/// # Arguments
/// - `manifest` (`&toml::Table`): das Paket-Manifest.
/// - `workspace` (`Option<&toml::Table>`): `[workspace.dependencies]` der
///   Wurzel, falls vorhanden.
/// - `feature` (`&str`): das geprüfte Feature.
/// - `scope` (`Scope`): ob Anforderungen aus Dev-Abhängigkeiten mitzählen.
///   `[features]`-Weiterleitungen zählen immer.
///
/// # Returns
/// Je Fundstelle eine lesbare Ortsangabe, getrennt nach
/// Abhängigkeitseinträgen und `[features]` ([`Requests`]).
fn member_requests(
    manifest: &toml::Table,
    workspace: Option<&toml::Table>,
    feature: &str,
    scope: Scope,
) -> Requests {
    let mut requests = Requests::default();
    // Unter diesen Namen ist `harw-dod-rules` in diesem Manifest erreichbar
    // (Schlüssel, lokale oder geerbte Umbenennung, auch aus
    // Dev-Abhängigkeiten); der Paketname selbst zählt immer.
    let mut aliases: BTreeSet<String> = BTreeSet::from([FEATURE_CRATE.to_owned()]);

    for DependencyTable {
        context,
        dev,
        table,
    } in dependency_tables(manifest)
    {
        let reported = scope == Scope::AllTables || !dev;
        for (key, value) in table {
            let dependency = resolve_dependency(key, value, workspace, feature);
            if dependency.targets_feature_crate {
                aliases.insert(key.clone());
            }
            if !reported {
                continue;
            }
            if dependency.requests_locally {
                requests.dependencies.push(format!("{context}.{key}"));
            } else if dependency.requests_by_inheritance {
                requests.dependencies.push(format!(
                    "{context}.{key} (erbt die features von [workspace.dependencies].{key})"
                ));
            }
        }
    }

    if let Some(features) = manifest.get("features").and_then(toml::Value::as_table) {
        for (name, enables) in features {
            let Some(enables) = enables.as_array() else {
                continue;
            };
            for entry in enables.iter().filter_map(toml::Value::as_str) {
                if enables_via_alias(entry, feature, &aliases) {
                    requests
                        .features
                        .push(format!("[features].{name} = \"{entry}\""));
                }
            }
        }
    }

    requests
}

/// Fundstellen, an denen ein Paket (aufgelöst, in irgendeiner
/// Abhängigkeitstabelle) von einem der [`ALLOWED_ENABLERS`] abhängt
/// (Regel (f)), je mit dem Namen des Escalators.
fn enabler_dependencies(
    manifest: &toml::Table,
    workspace: Option<&toml::Table>,
) -> Vec<(String, &'static str)> {
    let mut locations = Vec::new();
    for DependencyTable { context, table, .. } in dependency_tables(manifest) {
        for (key, value) in table {
            let enabler = ALLOWED_ENABLERS
                .iter()
                .find(|enabler| names_package(key, value, workspace, enabler))
                .copied();
            if let Some(enabler) = enabler {
                locations.push((format!("{context}.{key}"), enabler));
            }
        }
    }
    locations
}

/// Ob ein `[features]`-Eintrag `<alias>/<feature>` oder
/// `<alias>?/<feature>` für einen der `aliases` ist.
fn enables_via_alias(entry: &str, feature: &str, aliases: &BTreeSet<String>) -> bool {
    let Some((dependency, requested)) = entry.split_once('/') else {
        return false;
    };
    let dependency = dependency.strip_suffix('?').unwrap_or(dependency);
    requested == feature && aliases.contains(dependency)
}

/// Ob ein Eintrag einer `[features]`-Liste von [`FEATURE_CRATE`] das Feature
/// `feature` mit einschaltet.
///
/// # Description
/// Gezählt werden `"<feature>"`, `"dep:<feature>"` und `"<feature>/…"` bzw.
/// `"<feature>?/…"` — die letzten drei für das implizite Feature einer
/// gleichnamigen optionalen Abhängigkeit (siehe [`feature_declarations`]).
/// `?` und `dep:` schalten das implizite Feature nach Cargo-Regeln nicht in
/// jedem Fall ein; das Gate zählt sie trotzdem (konservativ).
fn implies_feature(entry: &str, feature: &str) -> bool {
    if entry == feature || entry.strip_prefix("dep:") == Some(feature) {
        return true;
    }
    entry.split_once('/').is_some_and(|(dependency, _)| {
        dependency.strip_suffix('?').unwrap_or(dependency) == feature
    })
}

/// Verstöße in [`FEATURE_CRATE`] selbst: ohne Escalator keine Deklaration
/// von [`FEATURE`] (Regel (b)), nie ein anderes Feature (auch nicht
/// `default`), das [`FEATURE`] (Regel (c)) oder [`TEST_SUPPORT_FEATURE`]
/// (Regel (d)) mit einschaltet, und kein Feature außerhalb von
/// [`OWNER_FEATURES`] (Regel (e)).
fn owner_violations(manifest: &toml::Table, escalator_present: bool) -> Vec<String> {
    let mut violations = Vec::new();
    let features = manifest.get("features").and_then(toml::Value::as_table);

    if !escalator_present {
        for declaration in feature_declarations(manifest, features) {
            violations.push(format!(
                "{FEATURE_CRATE}: {declaration} deklariert das Feature '{FEATURE}', aber kein \
                 Paket {} existiert; das Feature kehrt erst mit dem Escalator zurück \
                 (PL-100 WP 3.2), bis dahin gibt es den Operator-Pfad nur in Tests",
                ALLOWED_ENABLERS.join(", ")
            ));
        }
    }

    // Regel (e); [`FEATURE`] selbst regelt (b).
    let allowed = |name: &str| name == FEATURE || OWNER_FEATURES.contains(&name);
    for name in features.into_iter().flat_map(toml::Table::keys) {
        if !allowed(name.as_str()) {
            violations.push(format!(
                "{FEATURE_CRATE}: [features].{name} ist kein erlaubtes Feature (erlaubt: {}; \
                 {FEATURE} erst mit {}); unter einem frei gewählten Namen ließe sich der \
                 Operator-Pfad an den Regeln (b) und (c) vorbei freischalten (PL-100 WP0.11, \
                 Regel (e))",
                OWNER_FEATURES.join(", "),
                ALLOWED_ENABLERS.join(", ")
            ));
        }
    }
    for (location, name) in implicit_features(manifest, features) {
        if !allowed(name) {
            violations.push(format!(
                "{FEATURE_CRATE}: {location} (optional) legt das implizite Feature '{name}' an, \
                 das nicht erlaubt ist (erlaubt: {}); mit \"dep:{name}\" in einem erlaubten \
                 Feature unterdrücken oder nicht optional machen (PL-100 WP0.11, Regel (e))",
                OWNER_FEATURES.join(", ")
            ));
        }
    }

    let Some(features) = features else {
        return violations;
    };
    for guarded in [FEATURE, TEST_SUPPORT_FEATURE] {
        for (feature, enables) in features {
            if feature == guarded {
                continue;
            }
            let Some(enables) = enables.as_array() else {
                continue;
            };
            if enables
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|entry| implies_feature(entry, guarded))
            {
                violations.push(format!(
                    "{FEATURE_CRATE}: [features].{feature} schaltet '{guarded}' mit ein; jede \
                     Crate, die '{feature}' anfordert, bekäme es still mit"
                ));
            }
        }
    }
    violations
}

/// Wo [`FEATURE_CRATE`] ein Feature namens [`FEATURE`] deklariert: explizit
/// in `[features]` oder implizit als optionale Abhängigkeit dieses Namens
/// (Cargo legt dafür ein gleichnamiges Feature an; ob ein `dep:`-Eintrag das
/// unterdrückt, wertet das Gate hier bewusst nicht aus).
fn feature_declarations(manifest: &toml::Table, features: Option<&toml::Table>) -> Vec<String> {
    let mut declarations = Vec::new();
    if features.is_some_and(|features| features.contains_key(FEATURE)) {
        declarations.push(format!("[features].{FEATURE}"));
    }
    for DependencyTable { context, table, .. } in dependency_tables(manifest) {
        if table.get(FEATURE).is_some_and(is_optional) {
            declarations.push(format!(
                "{context}.{FEATURE} (optional, implizites Feature)"
            ));
        }
    }
    declarations
}

/// Die impliziten Features eines Manifests: je optionale Abhängigkeit ihr
/// Schlüssel samt Ort, sofern kein `[features]`-Eintrag `"dep:<Schlüssel>"`
/// sie unterdrückt (dann legt Cargo kein gleichnamiges Feature an).
fn implicit_features<'a>(
    manifest: &'a toml::Table,
    features: Option<&'a toml::Table>,
) -> Vec<(String, &'a str)> {
    let suppressed: BTreeSet<&str> = features
        .into_iter()
        .flat_map(toml::Table::values)
        .filter_map(toml::Value::as_array)
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter_map(|entry| entry.strip_prefix("dep:"))
        .collect();
    let mut implicit = Vec::new();
    for DependencyTable { context, table, .. } in dependency_tables(manifest) {
        for (key, value) in table {
            if is_optional(value) && !suppressed.contains(key.as_str()) {
                implicit.push((format!("{context}.{key}"), key.as_str()));
            }
        }
    }
    implicit
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

    /// Ein regelkonformes `harw-dod-rules`-Manifest: bis WP 3.2 ohne das
    /// Feature `operator-decision`.
    fn owner() -> TestResult<MemberManifest> {
        Ok((
            FEATURE_CRATE.to_owned(),
            table("[features]\ntest-support = []\n")?,
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

    /// Wie [`evaluate_members`], aber mit einer Wurzel-`Cargo.toml`.
    fn evaluate_with_root(root: &str, extra: Vec<MemberManifest>) -> TestResult<GateReport> {
        let mut members = vec![owner()?];
        members.extend(extra);
        Ok(evaluate(&table(root)?, &members))
    }

    /// Ob ein Verstoß mit `prefix` beginnt.
    fn has_violation(report: &GateReport, prefix: &str) -> bool {
        report.violations.iter().any(|v| v.starts_with(prefix))
    }

    /// Die Wurzel aus Befund 2 des Owner-Reviews: `rules` ist ein umbenannter
    /// Workspace-Eintrag auf `harw-dod-rules`, ohne das Feature.
    const RENAMED_ROOT: &str =
        "[workspace.dependencies]\nrules = { package = \"harw-dod-rules\", path = \"x\" }\n";

    /// Gegen den echten Workspace, als vollständiger Gate-Lauf (inklusive
    /// Gegenprobe gegen `Cargo.lock`): niemand schaltet das Feature ein, und
    /// `harw-dod-rules` deklariert es nicht (es gibt noch keinen Escalator).
    #[test]
    fn test_real_workspace_is_green() -> TestResult {
        let report = run_at(repo_root()?).map_err(TestError::Unexpected)?;
        assert!(report.is_green(), "{:?}", report.violations);
        assert!(report.checked > 1, "{report:?}");

        let (_, members) = load_manifests(repo_root()?).map_err(TestError::Unexpected)?;
        assert!(
            members.iter().any(|(name, _)| name == FEATURE_CRATE),
            "{FEATURE_CRATE} muss im Graphen stehen"
        );
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
        assert!(has_violation(&report, "harw-runtime: [dependencies]"));
        assert!(has_violation(
            &report,
            "harw-dod-escalate: [dev-dependencies]"
        ));
        assert!(has_violation(
            &report,
            "harw-ops: [target.'cfg(unix)'.dependencies]"
        ));
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
                 harmless = [\"dep:rules\"]\n",
            )?,
        ])?;
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(has_violation(&report, "harw-cli: [dependencies].rules"));
        assert!(has_violation(&report, "harw-tui: [features].confirm"));
        assert!(has_violation(&report, "harw-tui: [features].direct"));
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

    /// Owner-Review PR #168, Befund 2, wörtlich: die Wurzel benennt
    /// `harw-dod-rules` in `rules` um (ohne Feature), das Member erbt per
    /// `workspace = true` und fordert das Feature lokal an. Die frühere
    /// Fassung sah den Eintrag nicht (lokal kein `package`).
    #[test]
    fn test_renamed_workspace_dependency_from_owner_review_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            RENAMED_ROOT,
            vec![member(
                "harw-runtime",
                "[dependencies]\nrules = { workspace = true, features = [\"operator-decision\"] }\n",
            )?],
        )?;
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert!(
            has_violation(&report, "harw-runtime: [dependencies].rules schaltet"),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    /// Die Auflösung selbst, Lesart für Lesart.
    #[test]
    fn test_resolve_dependency_follows_workspace_renames() -> TestResult {
        let root = table(
            "[workspace.dependencies]\n\
             rules = { package = \"harw-dod-rules\", path = \"x\" }\n\
             armed = { package = \"harw-dod-rules\", path = \"x\", features = [\"operator-decision\"] }\n\
             other = { path = \"y\" }\n",
        )?;
        let workspace = workspace_dependencies(&root);
        // Ein einzelner Abhängigkeitseintrag als `toml::Value`.
        let entry = |text: &str| -> TestResult<toml::Value> {
            table(&format!("e = {text}\n"))?
                .get("e")
                .cloned()
                .ok_or(TestError::Missing("Eintrag e"))
        };

        // Owner-Fall: Alias nur über die Wurzel erkennbar, Feature lokal.
        let owner_case = resolve_dependency(
            "rules",
            &entry("{ workspace = true, features = [\"operator-decision\"] }")?,
            workspace,
            FEATURE,
        );
        assert_eq!(
            owner_case,
            ResolvedDependency {
                targets_feature_crate: true,
                requests_locally: true,
                requests_by_inheritance: false,
            }
        );

        // Geerbt ohne Feature: Alias ja, Anforderung nein.
        let plain =
            resolve_dependency("rules", &entry("{ workspace = true }")?, workspace, FEATURE);
        assert!(plain.targets_feature_crate);
        assert!(!plain.requests_locally);
        assert!(!plain.requests_by_inheritance);

        // Das Feature steht nur im Wurzeleintrag: geerbte Anforderung.
        let armed =
            resolve_dependency("armed", &entry("{ workspace = true }")?, workspace, FEATURE);
        assert!(armed.targets_feature_crate && armed.requests_by_inheritance);

        // Ohne `workspace = true` gilt die Wurzel nicht: `rules` ist dann
        // ein eigenes (fremdes) Paket namens `rules`.
        let local = resolve_dependency("rules", &entry("{ path = \"z\" }")?, workspace, FEATURE);
        assert!(!local.targets_feature_crate);

        // Lokales `package` neben `workspace = true` (von Cargo nicht
        // ausgewertet): konservativ trotzdem gezählt.
        let both = resolve_dependency(
            "other",
            &entry("{ workspace = true, package = \"harw-dod-rules\" }")?,
            workspace,
            FEATURE,
        );
        assert!(both.targets_feature_crate);

        // Fremdes Paket, kein Feature: nichts.
        let foreign =
            resolve_dependency("other", &entry("{ workspace = true }")?, workspace, FEATURE);
        assert!(!foreign.targets_feature_crate);
        assert!(!foreign.requests_locally);
        assert!(!foreign.requests_by_inheritance);

        // Dieselbe Auflösung für `test-support` (Regel (d)): das geprüfte
        // Feature ist ein Parameter, der Alias bleibt derselbe.
        let fixtures = resolve_dependency(
            "rules",
            &entry("{ workspace = true, features = [\"test-support\"] }")?,
            workspace,
            TEST_SUPPORT_FEATURE,
        );
        assert!(fixtures.targets_feature_crate && fixtures.requests_locally);
        let not_fixtures = resolve_dependency(
            "rules",
            &entry("{ workspace = true, features = [\"operator-decision\"] }")?,
            workspace,
            TEST_SUPPORT_FEATURE,
        );
        assert!(not_fixtures.targets_feature_crate);
        assert!(!not_fixtures.requests_locally && !not_fixtures.requests_by_inheritance);
        Ok(())
    }

    /// Das Feature steht nur im Wurzeleintrag; jedes erbende Member (auch
    /// umbenannt) bekommt es — Wurzel und Member sind rot.
    #[test]
    fn test_workspace_inherited_dependency_with_root_feature_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            "[workspace.dependencies]\n\
             harw-dod-rules = { path = \"x\", features = [\"operator-decision\"] }\n\
             rules = { package = \"harw-dod-rules\", path = \"x\", features = [\"operator-decision\"] }\n",
            vec![
                member(
                    "harw-runtime",
                    "[dependencies]\nharw-dod-rules = { workspace = true }\n",
                )?,
                member(
                    "harw-cli",
                    "[dev-dependencies]\nrules = { workspace = true }\n",
                )?,
            ],
        )?;
        assert_eq!(report.violations.len(), 4, "{:?}", report.violations);
        assert!(has_violation(
            &report,
            "Cargo.toml: [workspace.dependencies].harw-dod-rules"
        ));
        assert!(has_violation(
            &report,
            "Cargo.toml: [workspace.dependencies].rules"
        ));
        assert!(has_violation(
            &report,
            "harw-runtime: [dependencies].harw-dod-rules (erbt"
        ));
        assert!(has_violation(
            &report,
            "harw-cli: [dev-dependencies].rules (erbt"
        ));
        Ok(())
    }

    /// `[features]`-Weiterleitung über einen geerbten, umbenannten Alias,
    /// mit und ohne `?`; harmlose Einträge bleiben grün.
    #[test]
    fn test_feature_forwarding_through_an_inherited_alias_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            RENAMED_ROOT,
            vec![member(
                "harw-tui",
                "[dependencies]\nrules = { workspace = true, optional = true }\n\
                 [features]\nconfirm = [\"rules?/operator-decision\"]\n\
                 plain = [\"rules/operator-decision\"]\n\
                 harmless = [\"dep:rules\"]\n",
            )?],
        )?;
        assert_eq!(report.violations.len(), 2, "{:?}", report.violations);
        assert!(has_violation(
            &report,
            "harw-tui: [features].confirm = \"rules?/operator-decision\""
        ));
        assert!(has_violation(
            &report,
            "harw-tui: [features].plain = \"rules/operator-decision\""
        ));
        Ok(())
    }

    /// Geerbte, umbenannte Einträge in Dev-, Build- und Target-Tabellen.
    #[test]
    fn test_inherited_alias_in_dev_build_and_target_tables_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            RENAMED_ROOT,
            vec![member(
                "harw-ops",
                "[dev-dependencies]\nrules = { workspace = true, features = [\"operator-decision\"] }\n\
                 [build-dependencies]\nrules = { workspace = true, features = [\"operator-decision\"] }\n\
                 [target.'cfg(unix)'.dependencies]\nrules = { workspace = true, features = [\"operator-decision\"] }\n",
            )?],
        )?;
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(has_violation(&report, "harw-ops: [dev-dependencies].rules"));
        assert!(has_violation(
            &report,
            "harw-ops: [build-dependencies].rules"
        ));
        assert!(has_violation(
            &report,
            "harw-ops: [target.'cfg(unix)'.dependencies].rules"
        ));
        Ok(())
    }

    /// Kein Fehlalarm: geerbte, umbenannte Einträge ohne das Feature;
    /// `test-support` nur aus einer Dev-Abhängigkeit.
    #[test]
    fn test_inherited_alias_without_the_feature_is_green() -> TestResult {
        let report = evaluate_with_root(
            RENAMED_ROOT,
            vec![member(
                "harw-dod-escalate",
                "[dependencies]\nrules = { workspace = true }\n\
                 [dev-dependencies]\nrules = { workspace = true, features = [\"test-support\"] }\n",
            )?],
        )?;
        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    /// Weiterleitung durch eine Zwischen-Crate (`<x>/operator-decision` in
    /// den `features` einer anderen Abhängigkeit), lokal und in der Wurzel.
    #[test]
    fn test_forwarding_through_an_intermediate_dependency_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            "[workspace.dependencies]\n\
             harw-dod-sentinel = { path = \"y\", features = [\"rules?/operator-decision\"] }\n",
            vec![member(
                "harw-cli",
                "[dependencies]\n\
                 harw-dod-escalate = { path = \"x\", features = [\"harw-dod-rules/operator-decision\"] }\n\
                 harw-dod-sentinel = { workspace = true }\n",
            )?],
        )?;
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(has_violation(
            &report,
            "Cargo.toml: [workspace.dependencies].harw-dod-sentinel"
        ));
        assert!(has_violation(
            &report,
            "harw-cli: [dependencies].harw-dod-escalate schaltet"
        ));
        assert!(has_violation(
            &report,
            "harw-cli: [dependencies].harw-dod-sentinel (erbt"
        ));
        Ok(())
    }

    /// Regel (b): ohne Escalator ist das fehlende Feature der Sollzustand,
    /// eine Deklaration (explizit oder implizit) dagegen rot.
    #[test]
    fn test_owner_must_not_declare_the_feature_before_the_escalator() -> TestResult {
        let absent = evaluate(
            &toml::Table::new(),
            &[member(FEATURE_CRATE, "[features]\ntest-support = []\n")?],
        );
        assert!(absent.is_green(), "{:?}", absent.violations);

        let no_features = evaluate(&toml::Table::new(), &[member(FEATURE_CRATE, "")?]);
        assert!(no_features.is_green(), "{:?}", no_features.violations);

        let declared = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[features]\ntest-support = []\noperator-decision = []\n",
            )?],
        );
        assert_eq!(declared.violations.len(), 1, "{:?}", declared.violations);
        assert!(has_violation(
            &declared,
            "harw-dod-rules: [features].operator-decision deklariert"
        ));

        let implicit = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[dependencies]\noperator-decision = { package = \"x\", path = \"y\", optional = true }\n",
            )?],
        );
        assert_eq!(implicit.violations.len(), 1, "{:?}", implicit.violations);
        assert!(has_violation(
            &implicit,
            "harw-dod-rules: [dependencies].operator-decision (optional"
        ));
        Ok(())
    }

    /// Mit Escalator darf das Feature existieren und (nur) dort eingeschaltet
    /// werden — auch über einen geerbten, umbenannten Alias und aus
    /// Dev-Abhängigkeiten. Jede andere Crate bleibt rot.
    #[test]
    fn test_escalator_present_may_declare_and_enable_the_feature() -> TestResult {
        let root = table(RENAMED_ROOT)?;
        let declaring_owner = member(
            FEATURE_CRATE,
            "[features]\ntest-support = []\noperator-decision = []\n",
        )?;
        let escalator = member(
            "harw-escalator",
            "[dependencies]\nrules = { workspace = true, features = [\"operator-decision\"] }\n\
             [dev-dependencies]\nrules = { workspace = true, features = [\"operator-decision\", \"test-support\"] }\n",
        )?;

        let green = evaluate(&root, &[declaring_owner.clone(), escalator.clone()]);
        assert!(green.is_green(), "{:?}", green.violations);

        let intruder = member(
            "harw-runtime",
            "[dependencies]\nrules = { workspace = true, features = [\"operator-decision\"] }\n",
        )?;
        let red = evaluate(&root, &[declaring_owner, escalator, intruder]);
        assert_eq!(red.violations.len(), 1, "{:?}", red.violations);
        assert!(has_violation(&red, "harw-runtime: [dependencies].rules"));
        Ok(())
    }

    /// Regel (a), zweiter Absatz (Review-Befund Runde 2): auch der Escalator
    /// reicht das Feature nicht über eigene `[features]` weiter — sonst bekäme
    /// es jede Crate, die z. B. `harw-escalator/confirm` einschaltet, ohne dass
    /// ihr Manifest `/operator-decision` nennt.
    #[test]
    fn test_escalator_forwarding_the_feature_through_its_features_is_a_violation() -> TestResult {
        let root = table(RENAMED_ROOT)?;
        let declaring_owner = member(
            FEATURE_CRATE,
            "[features]\ntest-support = []\noperator-decision = []\n",
        )?;
        let escalator = member(
            "harw-escalator",
            "[dependencies]\nrules = { workspace = true, features = [\"operator-decision\"] }\n\
             [features]\nconfirm = [\"rules?/operator-decision\"]\n\
             direct = [\"harw-dod-rules/operator-decision\"]\n\
             unrelated = [\"rules/serde\"]\n",
        )?;
        let report = evaluate(&root, &[declaring_owner, escalator]);
        assert_eq!(report.violations.len(), 2, "{:?}", report.violations);
        assert!(has_violation(
            &report,
            "harw-escalator: [features].confirm = \"rules?/operator-decision\" reicht"
        ));
        assert!(has_violation(
            &report,
            "harw-escalator: [features].direct = \"harw-dod-rules/operator-decision\" reicht"
        ));
        Ok(())
    }

    /// Regel (f): kein anderes Paket hängt von `harw-escalator` ab — direkt,
    /// umbenannt, umbenannt-geerbt, in Dev- oder Target-Tabellen. Der
    /// Escalator selbst und ein gleichnamiger Fremd-Alias bleiben grün.
    #[test]
    fn test_depending_on_the_escalator_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            "[workspace.dependencies]\nesc = { package = \"harw-escalator\", path = \"e\" }\n\
             other = { path = \"o\" }\n",
            vec![
                member(
                    "harw-escalator",
                    "[dependencies]\nharw-dod-rules = { path = \"x\" }\n\
                     [dev-dependencies]\nharw-escalator = { path = \".\" }\n",
                )?,
                member(
                    "harw-runtime",
                    "[dependencies]\nesc = { workspace = true }\nother = { workspace = true }\n",
                )?,
                member(
                    "harw-cli",
                    "[dev-dependencies]\nharw-escalator = { path = \"e\" }\n",
                )?,
                member(
                    "harw-tui",
                    "[target.'cfg(unix)'.build-dependencies]\n\
                     escalator = { package = \"harw-escalator\", path = \"e\" }\n",
                )?,
                member("harw-ops", "[dependencies]\nesc = { path = \"z\" }\n")?,
            ],
        )?;
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(has_violation(
            &report,
            "harw-runtime: [dependencies].esc hängt von harw-escalator ab"
        ));
        assert!(has_violation(
            &report,
            "harw-cli: [dev-dependencies].harw-escalator hängt von harw-escalator ab"
        ));
        assert!(has_violation(
            &report,
            "harw-tui: [target.'cfg(unix)'.build-dependencies].escalator hängt von harw-escalator ab"
        ));
        Ok(())
    }

    /// Regel (e): `harw-dod-rules` deklariert nur `default`, `test-support`
    /// und (erst mit Escalator) `operator-decision` — explizit wie implizit.
    /// Ein frei benanntes Feature wie `human` wäre ein Weg, den Operator-Pfad
    /// an den Regeln (b)/(c) und den `compile_fail`-Doctests vorbei
    /// freizuschalten.
    #[test]
    fn test_owner_declares_only_allowed_features() -> TestResult {
        let allowed = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[dependencies]\nextra = { path = \"x\", optional = true }\n\
                 [features]\ndefault = []\ntest-support = [\"dep:extra\"]\n",
            )?],
        );
        assert!(allowed.is_green(), "{:?}", allowed.violations);

        let explicit = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[features]\ntest-support = []\nhuman = []\n",
            )?],
        );
        assert_eq!(explicit.violations.len(), 1, "{:?}", explicit.violations);
        assert!(has_violation(
            &explicit,
            "harw-dod-rules: [features].human ist kein erlaubtes Feature"
        ));

        let implicit = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[target.'cfg(unix)'.dependencies]\nhuman = { path = \"x\", optional = true }\n\
                 [features]\ntest-support = []\n",
            )?],
        );
        assert_eq!(implicit.violations.len(), 1, "{:?}", implicit.violations);
        assert!(has_violation(
            &implicit,
            "harw-dod-rules: [target.'cfg(unix)'.dependencies].human (optional) legt das implizite \
             Feature 'human' an"
        ));

        let with_escalator = evaluate(
            &toml::Table::new(),
            &[
                member(
                    FEATURE_CRATE,
                    "[features]\ntest-support = []\noperator-decision = []\nhuman = []\n",
                )?,
                member("harw-escalator", "")?,
            ],
        );
        assert_eq!(
            with_escalator.violations.len(),
            1,
            "{:?}",
            with_escalator.violations
        );
        assert!(has_violation(
            &with_escalator,
            "harw-dod-rules: [features].human ist kein erlaubtes Feature"
        ));
        Ok(())
    }

    /// Regel (c): kein anderes Feature von `harw-dod-rules` schaltet das
    /// Feature mit ein — mit und ohne Escalator.
    #[test]
    fn test_owner_never_implies_the_feature() -> TestResult {
        let implying = "[features]\ndefault = [\"operator-decision\"]\n\
                        test-support = [\"operator-decision\"]\noperator-decision = []\n";

        let without_escalator = evaluate(&toml::Table::new(), &[member(FEATURE_CRATE, implying)?]);
        assert_eq!(
            without_escalator.violations.len(),
            3,
            "{:?}",
            without_escalator.violations
        );
        let implied = without_escalator
            .violations
            .iter()
            .filter(|v| v.contains("mit ein"))
            .count();
        assert_eq!(implied, 2, "{:?}", without_escalator.violations);

        let with_escalator = evaluate(
            &toml::Table::new(),
            &[
                member(FEATURE_CRATE, implying)?,
                member("harw-escalator", "")?,
            ],
        );
        assert_eq!(
            with_escalator.violations.len(),
            2,
            "{:?}",
            with_escalator.violations
        );
        assert!(
            with_escalator
                .violations
                .iter()
                .all(|v| v.contains("mit ein"))
        );
        Ok(())
    }

    /// `harw-dod-rules` selbst ist nicht der Escalator: auch eine
    /// Selbst-Dev-Abhängigkeit mit dem Feature ist rot.
    #[test]
    fn test_owner_requesting_its_own_feature_is_a_violation() -> TestResult {
        let report = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[dev-dependencies]\nharw-dod-rules = { path = \".\", features = [\"operator-decision\"] }\n",
            )?],
        );
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert!(has_violation(
            &report,
            "harw-dod-rules: [dev-dependencies].harw-dod-rules"
        ));
        Ok(())
    }

    /// Eine Wurzel mit `[package]` ist ein Paket wie jedes Member — und wird
    /// nicht doppelt gezählt, wenn sie zugleich unter den Membern steht.
    #[test]
    fn test_root_package_is_checked_like_a_member() -> TestResult {
        let root_text = "[package]\nname = \"harw-root\"\n\
                         [workspace]\nmembers = []\n\
                         [dependencies]\nharw-dod-rules = { path = \"x\", features = [\"operator-decision\"] }\n";
        let root = table(root_text)?;

        let alone = evaluate(&root, &[owner()?]);
        assert_eq!(alone.violations.len(), 1, "{:?}", alone.violations);
        assert!(has_violation(
            &alone,
            "harw-root: [dependencies].harw-dod-rules"
        ));

        let listed = evaluate(&root, &[owner()?, member("harw-root", root_text)?]);
        assert_eq!(listed.violations.len(), 1, "{:?}", listed.violations);
        Ok(())
    }

    #[test]
    fn test_missing_owner_is_not_green() {
        let report = evaluate(&toml::Table::new(), &[]);
        assert!(!report.is_green());
        assert!(report.violations.iter().any(|v| v.contains("fehlt")));
    }

    /// Ob ein Verstoß mit `prefix` beginnt und Regel (d) meldet.
    fn has_test_support_violation(report: &GateReport, prefix: &str) -> bool {
        report
            .violations
            .iter()
            .any(|v| v.starts_with(prefix) && v.contains("harw-dod-rules/test-support"))
    }

    /// Regel (d): `test-support` aus einer normalen, Build- oder
    /// Target-Abhängigkeit erzeugte im produktiven Build ein `Confirmed`.
    #[test]
    fn test_test_support_outside_dev_dependencies_is_a_violation() -> TestResult {
        let report = evaluate_members(vec![
            member(
                "harw-runtime",
                "[dependencies]\nharw-dod-rules = { path = \"x\", features = [\"test-support\"] }\n",
            )?,
            member(
                "harw-cli",
                "[build-dependencies]\nharw-dod-rules = { path = \"x\", features = [\"test-support\"] }\n",
            )?,
            member(
                "harw-ops",
                "[target.'cfg(unix)'.dependencies]\nharw-dod-rules = { path = \"x\", features = [\"test-support\"] }\n",
            )?,
            member(
                "harw-tui",
                "[build_dependencies]\nrules = { package = \"harw-dod-rules\", path = \"x\", features = [\"test-support\"] }\n",
            )?,
        ])?;
        assert_eq!(report.violations.len(), 4, "{:?}", report.violations);
        assert!(has_test_support_violation(
            &report,
            "harw-runtime: [dependencies].harw-dod-rules schaltet"
        ));
        assert!(has_test_support_violation(
            &report,
            "harw-cli: [build-dependencies].harw-dod-rules schaltet"
        ));
        assert!(has_test_support_violation(
            &report,
            "harw-ops: [target.'cfg(unix)'.dependencies].harw-dod-rules schaltet"
        ));
        assert!(has_test_support_violation(
            &report,
            "harw-tui: [build_dependencies].rules schaltet"
        ));
        Ok(())
    }

    /// Regel (d) in der Form des Owner-Befunds 2: umbenannt-geerbter
    /// Eintrag in `[dependencies]`, `test-support` lokal angefordert.
    #[test]
    fn test_test_support_through_a_renamed_inherited_dependency_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            RENAMED_ROOT,
            vec![member(
                "harw-runtime",
                "[dependencies]\nrules = { workspace = true, features = [\"test-support\"] }\n",
            )?],
        )?;
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert!(
            has_test_support_violation(&report, "harw-runtime: [dependencies].rules schaltet"),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    /// `test-support` im Wurzeleintrag: die Wurzel ist rot, ein erbendes
    /// `[dependencies]`-Member ebenfalls, ein erbendes Dev-Member nicht.
    #[test]
    fn test_workspace_dependency_with_test_support_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            "[workspace.dependencies]\n\
             rules = { package = \"harw-dod-rules\", path = \"x\", features = [\"test-support\"] }\n",
            vec![
                member(
                    "harw-cli",
                    "[dev-dependencies]\nrules = { workspace = true }\n",
                )?,
                member(
                    "harw-runtime",
                    "[dependencies]\nrules = { workspace = true }\n",
                )?,
            ],
        )?;
        assert_eq!(report.violations.len(), 2, "{:?}", report.violations);
        assert!(has_test_support_violation(
            &report,
            "Cargo.toml: [workspace.dependencies].rules"
        ));
        assert!(has_test_support_violation(
            &report,
            "harw-runtime: [dependencies].rules (erbt"
        ));
        Ok(())
    }

    /// `[features]`-Weiterleitung von `test-support` über einen geerbten
    /// Alias (mit und ohne `?`) und über den Paketnamen ist rot — auch wenn
    /// das Manifest `test-support` zusätzlich korrekt als Dev-Abhängigkeit
    /// anfordert. Die Weiterleitung landet sonst bei jedem Konsumenten, der
    /// das Feature einschaltet.
    #[test]
    fn test_test_support_forwarded_through_features_is_a_violation() -> TestResult {
        let report = evaluate_with_root(
            RENAMED_ROOT,
            vec![member(
                "harw-dod-escalate",
                "[dependencies]\nrules = { workspace = true }\n\
                 [dev-dependencies]\nrules = { workspace = true, features = [\"test-support\"] }\n\
                 [features]\nfixtures = [\"rules/test-support\"]\n\
                 maybe = [\"rules?/test-support\"]\n\
                 direct = [\"harw-dod-rules/test-support\"]\n\
                 harmless = [\"dep:rules\", \"rules/serde\"]\n",
            )?],
        )?;
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(has_test_support_violation(
            &report,
            "harw-dod-escalate: [features].fixtures = \"rules/test-support\""
        ));
        assert!(has_test_support_violation(
            &report,
            "harw-dod-escalate: [features].maybe = \"rules?/test-support\""
        ));
        assert!(has_test_support_violation(
            &report,
            "harw-dod-escalate: [features].direct = \"harw-dod-rules/test-support\""
        ));
        Ok(())
    }

    /// Weiterleitung `<x>/test-support` an einer Zwischen-Crate: außerhalb
    /// von Dev-Abhängigkeiten rot, in Dev-Abhängigkeiten grün.
    #[test]
    fn test_test_support_forwarded_through_an_intermediate_dependency() -> TestResult {
        let red = evaluate_members(vec![member(
            "harw-cli",
            "[dependencies]\nharw-dod = { path = \"x\", features = [\"harw-dod-rules/test-support\"] }\n",
        )?])?;
        assert_eq!(red.violations.len(), 1, "{:?}", red.violations);
        assert!(has_test_support_violation(
            &red,
            "harw-cli: [dependencies].harw-dod schaltet"
        ));

        let green = evaluate_members(vec![member(
            "harw-cli",
            "[dev-dependencies]\nharw-dod = { path = \"x\", features = [\"harw-dod-rules/test-support\"] }\n",
        )?])?;
        assert!(green.is_green(), "{:?}", green.violations);
        Ok(())
    }

    /// Kein Fehlalarm: `test-support` aus jeder Form einer
    /// Dev-Abhängigkeit (direkt, umbenannt-geerbt, Unterstrich-Schreibweise,
    /// Target-Tabelle) ist der Sollzustand; das gleichnamige Feature einer
    /// anderen Crate zählt nicht.
    #[test]
    fn test_test_support_from_dev_dependencies_is_green() -> TestResult {
        let report = evaluate_with_root(
            RENAMED_ROOT,
            vec![
                member(
                    "harw-dod",
                    "[dependencies]\nharw-dod-rules = { path = \"x\" }\n\
                     [dev-dependencies]\nharw-dod-rules = { path = \"x\", features = [\"test-support\"] }\n",
                )?,
                member(
                    "harw-dod-escalate",
                    "[dependencies]\nrules = { workspace = true }\n\
                     [dev-dependencies]\nrules = { workspace = true, features = [\"test-support\"] }\n",
                )?,
                member(
                    "harw-plan-bridge",
                    "[dev_dependencies]\nharw-dod-rules = { path = \"x\", features = [\"test-support\"] }\n",
                )?,
                member(
                    "harw-ops",
                    "[target.'cfg(unix)'.dev-dependencies]\nrules = { workspace = true, features = [\"test-support\"] }\n",
                )?,
                member(
                    "harw-tools",
                    "[dependencies]\nharw-authority = { path = \"y\", features = [\"test-support\"] }\n",
                )?,
            ],
        )?;
        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    /// Regel (d) gilt auch für den Escalator: er darf `operator-decision`
    /// anfordern, `test-support` aber nur als Dev-Abhängigkeit.
    #[test]
    fn test_escalator_is_not_exempt_from_the_test_support_rule() -> TestResult {
        let report = evaluate_members(vec![member(
            "harw-escalator",
            "[dependencies]\nharw-dod-rules = { path = \"x\", features = [\"operator-decision\", \"test-support\"] }\n",
        )?])?;
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert!(has_test_support_violation(
            &report,
            "harw-escalator: [dependencies].harw-dod-rules schaltet"
        ));
        Ok(())
    }

    /// Regel (d) im Eigentümer: kein anderes Feature (auch nicht `default`)
    /// schaltet `test-support` mit ein. `extra` verletzt zusätzlich die
    /// Allowlist (Regel (e)).
    #[test]
    fn test_owner_never_implies_test_support() -> TestResult {
        let report = evaluate(
            &toml::Table::new(),
            &[member(
                FEATURE_CRATE,
                "[features]\ndefault = [\"test-support\"]\ntest-support = []\n\
                 extra = [\"dep:test-support\"]\n",
            )?],
        );
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(has_violation(
            &report,
            "harw-dod-rules: [features].default schaltet 'test-support' mit ein"
        ));
        assert!(has_violation(
            &report,
            "harw-dod-rules: [features].extra schaltet 'test-support' mit ein"
        ));
        assert!(has_violation(
            &report,
            "harw-dod-rules: [features].extra ist kein erlaubtes Feature"
        ));
        Ok(())
    }

    /// Regel (c) mit Escalator: eine optionale Abhängigkeit namens
    /// `operator-decision` ist ein implizites Feature; ein anderes Feature,
    /// das sie über `"operator-decision/x"`, `"operator-decision?/x"` oder
    /// `"dep:operator-decision"` einschaltet, ist rot.
    #[test]
    fn test_owner_implying_the_implicit_feature_is_a_violation() -> TestResult {
        let escalator = member("harw-escalator", "")?;
        let exact = evaluate(
            &toml::Table::new(),
            &[
                member(
                    FEATURE_CRATE,
                    "[dependencies]\noperator-decision = { package = \"x\", path = \"y\", optional = true }\n\
                     [features]\ntest-support = [\"operator-decision/x\"]\n",
                )?,
                escalator.clone(),
            ],
        );
        assert_eq!(exact.violations.len(), 1, "{:?}", exact.violations);
        assert!(has_violation(
            &exact,
            "harw-dod-rules: [features].test-support schaltet 'operator-decision' mit ein"
        ));

        // Nur Feature-Namen aus der Allowlist (Regel (e)), damit allein
        // Regel (c) zählt.
        let variants = evaluate(
            &toml::Table::new(),
            &[
                member(
                    FEATURE_CRATE,
                    "[dependencies]\noperator-decision = { package = \"x\", path = \"y\", optional = true }\n\
                     [features]\ntest-support = [\"operator-decision?/x\"]\n\
                     default = [\"dep:operator-decision\"]\n",
                )?,
                escalator,
            ],
        );
        assert_eq!(variants.violations.len(), 2, "{:?}", variants.violations);
        assert!(has_violation(
            &variants,
            "harw-dod-rules: [features].default schaltet 'operator-decision' mit ein"
        ));
        assert!(has_violation(
            &variants,
            "harw-dod-rules: [features].test-support schaltet 'operator-decision' mit ein"
        ));
        Ok(())
    }

    /// Die Erkennung impliziter Einschaltungen selbst.
    #[test]
    fn test_implies_feature_forms() {
        for entry in [
            "operator-decision",
            "dep:operator-decision",
            "operator-decision/x",
            "operator-decision?/x",
        ] {
            assert!(implies_feature(entry, FEATURE), "{entry}");
        }
        for entry in [
            "test-support",
            "rules/operator-decision",
            "dep:rules",
            "operator-decision-extra",
            "x/operator-decision/y",
        ] {
            assert!(!implies_feature(entry, FEATURE), "{entry}");
        }
    }

    /// Legt `dir` an und schreibt `text` als `dir/Cargo.toml`.
    fn write_manifest(dir: &Path, text: &str) -> TestResult {
        fs::create_dir_all(dir).map_err(ctx("Scratch-Verzeichnis anlegen"))?;
        fs::write(dir.join("Cargo.toml"), text).map_err(ctx("Cargo.toml schreiben"))?;
        Ok(())
    }

    /// Ein Scratch-Verzeichnis je Test und Prozess (Stil der Warden-Gates).
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gate-operator-decision-{name}-{}",
            std::process::id()
        ));
        // Reste eines früheren Laufs mit derselben PID; ein Fehlschlag ist
        // unschädlich, weil jeder Test jede Datei, die er liest, neu schreibt.
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// Review-Befund Runde 2: Pakete außerhalb von `members`, die nur über
    /// `path` erreichbar sind — eines im Workspace-Verzeichnis (für Cargo ein
    /// implizites Member), eines außerhalb und nur transitiv erreichbar —
    /// werden gelesen und wie Member geprüft; jedes Manifest genau einmal.
    #[test]
    fn test_unlisted_path_packages_are_checked() -> TestResult {
        let base = scratch("paths");
        let workspace = base.join("ws");
        write_manifest(
            &workspace,
            "[workspace]\nmembers = [\"app\", \"harw-dod-rules\"]\n\n\
             [workspace.dependencies]\nrules = { package = \"harw-dod-rules\", path = \"harw-dod-rules\" }\n",
        )?;
        write_manifest(
            &workspace.join("harw-dod-rules"),
            "[package]\nname = \"harw-dod-rules\"\n\n[features]\ntest-support = []\n",
        )?;
        write_manifest(
            &workspace.join("app"),
            "[package]\nname = \"app\"\n\n\
             [dependencies]\nfoo = { path = \"../foo\" }\nrules = { workspace = true }\n",
        )?;
        write_manifest(
            &workspace.join("foo"),
            "[package]\nname = \"foo\"\n\n\
             [dependencies]\nrules = { workspace = true, features = [\"test-support\"] }\n\
             bar = { path = \"../../bar\" }\n",
        )?;
        write_manifest(
            &base.join("bar"),
            "[package]\nname = \"bar\"\n\n\
             [build-dependencies]\nharw-dod-rules = { path = \"../ws/harw-dod-rules\", features = [\"operator-decision\"] }\n",
        )?;
        fs::write(
            workspace.join("Cargo.lock"),
            "version = 4\n\n\
             [[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [[package]]\nname = \"bar\"\nversion = \"0.1.0\"\n\n\
             [[package]]\nname = \"foo\"\nversion = \"0.1.0\"\n\n\
             [[package]]\nname = \"harw-dod-rules\"\nversion = \"0.1.0\"\n",
        )
        .map_err(ctx("Cargo.lock schreiben"))?;

        let (_, packages) = load_manifests(&workspace).map_err(TestError::Unexpected)?;
        let names: BTreeSet<&str> = packages.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["app", "bar", "foo", "harw-dod-rules"]),
            "{names:?}"
        );
        assert_eq!(packages.len(), 4, "jedes Manifest genau einmal: {names:?}");

        let report = run_at(&workspace).map_err(TestError::Unexpected)?;
        assert_eq!(report.violations.len(), 2, "{:?}", report.violations);
        assert!(
            has_test_support_violation(&report, "foo: [dependencies].rules schaltet"),
            "{:?}",
            report.violations
        );
        assert!(
            has_violation(
                &report,
                "bar: [build-dependencies].harw-dod-rules schaltet harw-dod-rules/operator-decision"
            ),
            "{:?}",
            report.violations
        );

        // Aufräumen ist nur Hygiene; ein Rest stört keinen späteren Lauf.
        let _ = fs::remove_dir_all(&base);
        Ok(())
    }

    /// Fehlt das Ziel einer `path`-Abhängigkeit, ist das ein Fehler (wie für
    /// Cargo), kein stilles Überspringen.
    #[test]
    fn test_missing_path_dependency_is_an_error() -> TestResult {
        let base = scratch("missing");
        write_manifest(&base, "[workspace]\nmembers = [\"app\"]\n")?;
        write_manifest(
            &base.join("app"),
            "[package]\nname = \"app\"\n\n[dependencies]\nghost = { path = \"../ghost\" }\n",
        )?;
        let loaded = load_manifests(&base);
        assert!(loaded.is_err(), "ein fehlendes path-Ziel muss Err sein");
        // Aufräumen ist nur Hygiene; ein Rest stört keinen späteren Lauf.
        let _ = fs::remove_dir_all(&base);
        Ok(())
    }

    /// Welche `path`-Einträge gefolgt wird: in jedem Paket alle
    /// Abhängigkeitstabellen, in der Wurzel zusätzlich
    /// `[workspace.dependencies]`, `[patch.*]` und `[replace]`.
    #[test]
    fn test_path_dependency_dirs_reads_every_section() -> TestResult {
        let manifest = table(
            "[dev-dependencies]\nf = { path = \"df\" }\nplain = \"1\"\n\
             [target.'cfg(unix)'.dependencies]\ne = { path = \"te\" }\n\
             [workspace.dependencies]\na = { path = \"wa\" }\nb = \"1\"\n\
             [patch.crates-io]\nc = { path = \"pc\" }\n\
             [replace]\n\"d:1.0.0\" = { path = \"rd\" }\n",
        )?;
        let base = Path::new("base");

        let mut as_root = path_dependency_dirs(base, &manifest, ManifestRole::Root);
        as_root.sort();
        assert_eq!(
            as_root,
            vec![
                base.join("df"),
                base.join("pc"),
                base.join("rd"),
                base.join("te"),
                base.join("wa"),
            ]
        );

        let mut as_package = path_dependency_dirs(base, &manifest, ManifestRole::Package);
        as_package.sort();
        assert_eq!(as_package, vec![base.join("df"), base.join("te")]);
        Ok(())
    }

    /// Regel (g): jedes lokale Paket aus `Cargo.lock` (ohne `source`) muss
    /// gelesen worden sein; Registry-Pakete und die Wurzel zählen nicht.
    #[test]
    fn test_lock_coverage_reports_unread_local_packages() -> TestResult {
        let lock = "version = 4\n\n\
                    [[package]]\nname = \"harw-dod-rules\"\nversion = \"0.1.0\"\n\n\
                    [[package]]\nname = \"ghost\"\nversion = \"0.1.0\"\n\n\
                    [[package]]\nname = \"harw-root\"\nversion = \"0.1.0\"\n\n\
                    [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let root = table("[package]\nname = \"harw-root\"\n")?;

        let violations = lock_coverage(lock, &root, &[owner()?]).map_err(TestError::Unexpected)?;
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations
                .iter()
                .all(|v| v.starts_with("ghost: lokales Paket laut Cargo.lock")),
            "{violations:?}"
        );

        let complete = lock_coverage(
            lock,
            &root,
            &[owner()?, member("ghost", "[package]\nname = \"ghost\"\n")?],
        )
        .map_err(TestError::Unexpected)?;
        assert!(complete.is_empty(), "{complete:?}");

        // Ein lokales `[[package]]` ohne Namen ist ein Fehler, kein stilles
        // Überspringen.
        assert!(lock_coverage("[[package]]\nversion = \"1\"\n", &toml::Table::new(), &[]).is_err());
        Ok(())
    }
}
