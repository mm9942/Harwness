//! Gate `arch`: Schichtenregeln, TCB-Allowlisten und `*-sys`-Verbot.
//!
//! # Verantwortungsbereich
//! Prüft den Abhängigkeitsgraphen gegen die maschinenlesbare Richtlinie
//! [`POLICY_PATH`] (Architekturplan
//! `docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md`,
//! §6, §23, §40–§43, §47, §55, §56). Die Regeln stehen als **Daten** in der
//! TOML-Datei, nicht im Code: eine neue Crate zu klassifizieren oder eine
//! Inversion zu dulden ist ein Eintrag dort, keine neue Funktion hier.
//!
//! # Die Prüfungen
//! 1. **Klassifikation.** Jedes Paket im Graphen muss unter `[packages]`
//!    stehen. Eine neue, nicht klassifizierte Crate ist ein Verstoß — sonst
//!    wüchse der Graph an der Richtlinie vorbei. Umgekehrt dürfen dort
//!    Pakete stehen, die es (noch) nicht gibt: angekündigte Crates werden
//!    vorab eingeordnet.
//! 2. **Schichtregel.** Jede interne *normale* Kante (`[dependencies]`,
//!    weder dev noch build) muss nach `[rules]` erlaubt sein — oder als
//!    `[[exceptions]]` mit Grund und Zieltermin dastehen.
//! 3. **Ratsche.** Eine Ausnahme, zu der keine Kante mehr existiert, oder
//!    eine, deren Kante die Regeln ohnehin erlauben, ist ein Verstoß. Die
//!    Ausnahmeliste kann dadurch nur schrumpfen, nie still veralten.
//! 4. **TCB-Allowlisten.** Ein Paket mit `tcb = true` darf direkt nur auf
//!    die in `[tcb."<name>"].allowed_internal` genannten internen Crates
//!    zeigen (§23: „Warden may depend only on allowlisted crates"). Eine
//!    Allowlist-Zeile ohne Kante ist ebenfalls veraltet (Ratsche).
//! 5. **`*-sys`-Verbot.** Schichten mit `forbid_sys_crates = true` und —
//!    bei `[sys_crates].forbid_for_tcb` — jedes TCB-Paket dürfen in ihrer
//!    transitiven Hülle (einschließlich `[build-dependencies]`) keine Crate
//!    erreichen, deren Name auf `-sys` endet, außer den unter
//!    `[[sys_crates.allow]]` begründeten (§40: native Abhängigkeiten dürfen
//!    nicht über Feature-Vereinigung einwandern).
//!    Auch hier gilt die Ratsche für ungenutzte Allowlist-Einträge.
//!    Dieselbe Prüfung erfasst auch [`C_BUILD_HELPERS`]: C/Asm-Bauhelfer ohne
//!    `-sys`-Namen (`ring`, `cmake`, `bindgen`, `cc`; Tier C der
//!    `dependency-review.md`), die der Namens-Suffix allein nicht fände. Für
//!    `cc` gibt es keinen Allowlist-Eintrag, sondern eine fest codierte,
//!    begründete Ausnahme (siehe [`CC_EXCEPTION_VIA`]): erlaubt nur, wenn
//!    *jeder* Pfad zu `cc` über `blake3` läuft, kein Blanket-Allow.
//! 6. **Verbotene Crates.** Ein Paket, für das eine `[[forbidden_crates]]`-
//!    Zeile gilt (per Schicht, per Name oder als TCB-Paket), darf keinen der
//!    dort genannten Namen transitiv erreichen — intern wie extern. Die Hülle
//!    dafür liest die Manifeste selbst ([`ManifestEdges`], siehe den
//!    übernächsten Abschnitt), nicht nur den [`WorkspaceGraph`]. Ein Name in
//!    einer solchen Hülle, der weder lokales Paket noch in `Cargo.lock`
//!    verzeichnet ist, ist ein Verstoß (fail closed), für **jedes** Paket,
//!    nicht nur für J/TCB: sonst schlüpfte etwa eine umbenannte Kante als
//!    unbekannter Name an der Prüfung vorbei.
//!
//! # Wie die `*-sys`-Hülle entsteht
//! Innerhalb des Workspace folgt sie den internen und externen
//! `[dependencies]` **und** den `[build-dependencies]` aus dem
//! [`WorkspaceGraph`]: ein Build-Skript übersetzt C-Code (`cc`, `bindgen`,
//! `*-sys`) in genau die Crate, die es baut — dort kommt ein C-Bauhelfer
//! typischerweise herein. `[dev-dependencies]` zählen nicht, sie landen in
//! keinem ausgelieferten Artefakt. Ab einer externen Crate folgt sie den
//! `dependencies`-Listen in `Cargo.lock`. `Cargo.lock` kennt weder
//! Plattform- noch Feature-Filter und führt auch Build-Abhängigkeiten
//! externer Crates; die Hülle ist deshalb eine **Überschätzung** dessen, was
//! der Graph sieht — sie meldet etwa `windows-sys`, das auf Linux nie gebaut
//! wird. Solche Fälle stehen begründet in der Allowlist. Blind ist sie für
//! Target-Tabellen (siehe den nächsten Abschnitt). Eine externe Crate ohne
//! `Cargo.lock`-Eintrag ist ein Verstoß: die Hülle wäre sonst still
//! unvollständig.
//!
//! # Wie die `[[forbidden_crates]]`-Hülle entsteht (PL-100 WP 3.0, B16)
//! Die Verbots-Zeilen tragen Leitprinzip I8 („Prozesse, die mit Modellen
//! sprechen, linken keinen Signier-Code"); README §4, 3.0 verlangt dafür eine
//! Prüfung wie `cargo tree`, also auf der **aufgelösten** Paketidentität. Der
//! [`WorkspaceGraph`] kennt dagegen nur die Schlüssel aus `[dependencies]` und
//! `[build-dependencies]`: eine umbenannte Kante
//! (`signing = { package = "harw-dod-warden-proto", … }`) hieße dort
//! `signing`, und eine Kante nur unter `[target.'cfg(..)'.dependencies]`
//! fehlte ganz. Für lokale Pakete folgt diese Hülle deshalb
//! [`ManifestEdges`]: jede Nicht-Dev-Tabelle (`[dependencies]`,
//! `[build-dependencies]`, die alte Unterstrich-Schreibweise und jede
//! `[target.'…']`-Variante), jeder Eintrag unter seinem Paketnamen
//! (`package = "…"`, mit `workspace = true` der des Wurzeleintrags; ein
//! lokales `package` daneben zählt konservativ mit). Gelesen werden dieselben
//! Manifeste wie im Gate `operator-decision` (Wurzel, Member und jedes über
//! `path` erreichte Paket). Ab einer externen Crate folgt sie `Cargo.lock`,
//! das Paketnamen führt und keine Plattformfilter kennt. Optionale
//! Abhängigkeiten zählen immer mit (Überschätzung, wie bei `Cargo.lock`).
//!
//! # Was dieses Gate nicht prüft
//! `[target.'cfg(..)'.dependencies]` samt den `build-`/`dev-`Varianten sieht
//! der [`WorkspaceGraph`] nicht — dieselbe Grenze wie bei `edges` und
//! `privileges`. Dort deklarierte Kanten fehlen der Schichtregel, den
//! TCB-Allowlisten **und** der `*-sys`-Hülle (nicht aber der
//! `[[forbidden_crates]]`-Hülle, siehe oben): bei einer J-Crate, die ihre
//! plattformspezifischen Abhängigkeiten nur dort deklariert (etwa
//! `harw-job-linux` mit `rustix`/`landlock`), prüft die `*-sys`-Regel genau
//! diese Kanten nicht; ihren `[dependencies]` folgt die Hülle weiterhin.
//! Schließen lässt sich das erst mit Target-Kanten im Graphen
//! (`harw-code-graph`). Und es prüft Kanten, keine Aufrufe.
//!
//! Ebenso wenig sieht es Quelltext, der ohne Kante in ein Paket gelangt:
//! `#[path = "../…"] mod x;` oder `include!("../…")` übersetzen Code eines
//! anderen Pakets mit, ohne dass eine `[[forbidden_crates]]`-Zeile
//! anschlägt. Für Leitprinzip I8 (modellnahe Prozesse linken keinen
//! Signier-Code) ist das die verbleibende Lücke. Stand PL-100 WP 3.0 nutzt
//! kein Paket der I8-Zeile `#[path]` oder `include!` mit einem Pfad
//! außerhalb seines Verzeichnisses (die `include_str!`-Szenarien in
//! `harw-ops` sind TOML-Daten, kein Code); ein Gate dafür gibt es nicht.
//!
//! # Formfehler der Richtlinie
//! Eine unlesbare oder in sich widersprüchliche Richtlinie (unbekannte
//! Schicht, Tippfehler in einem Schlüssel, `tcb = true` ohne Allowlist, …)
//! ist ein `Err`, kein Verstoß: das Gate kann dann gar nichts prüfen.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

use harw_code_graph::{CrateNode, WorkspaceGraph};

use super::GateReport;

/// Pfad der Richtlinie, relativ zur Repo-Wurzel.
pub const POLICY_PATH: &str = "xtask/arch-policy.toml";

/// Platzhalter in `may_depend_on`: jede Schicht ist erlaubt.
const WILDCARD: &str = "*";

/// Namensendung, an der eine Crate mit (möglicher) C-Bindung erkannt wird.
const SYS_SUFFIX: &str = "-sys";

/// C/Asm-Bauhelfer ohne `-sys`-Namensendung (Tier C,
/// `docs/architecture/dependency-review.md`): `cargo tree -i <name>` findet
/// sie, der `-sys`-Suffix-Test allein nicht. Werden wie `*-sys`-Crates in
/// [`sys_hull`] erfasst und in [`evaluate_with_manifests`] geprüft.
const C_BUILD_HELPERS: &[&str] = &["ring", "cmake", "bindgen", "cc"];

/// Name der Crate, für die es — statt eines Allowlist-Eintrags — eine fest
/// codierte Ausnahme gibt.
const CC: &str = "cc";

/// Die einzige Zwischenstation, über die `cc` ein J/TCB-Paket erreichen darf
/// (Tier A derselben Doku: `harw-types`/`harw-digest` → `blake3` → `cc`).
///
/// # Description
/// Kein Blanket-Allow von `cc`: die Ausnahme gilt nur, wenn **jeder** Pfad
/// zu `cc` über [`CC_EXCEPTION_VIA`] läuft. [`evaluate_with_manifests`] prüft
/// das, indem es die Hülle ein zweites Mal bildet, ohne durch
/// [`CC_EXCEPTION_VIA`] zu treten ([`sys_hull_excluding`]) — bleibt `cc`
/// darin erreichbar, führt ein Pfad an `blake3` vorbei, und es ist ein
/// Verstoß.
const CC_EXCEPTION_VIA: &str = "blake3";

/// Ob `name` als `*-sys`-Crate oder als C-Bauhelfer ohne `-sys`-Namen zählt.
fn is_native_build_crate(name: &str) -> bool {
    name.ends_with(SYS_SUFFIX) || C_BUILD_HELPERS.contains(&name)
}

/// Die Regel einer Schicht.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayerRule {
    /// Schichten, auf die gezeigt werden darf.
    may_depend_on: BTreeSet<String>,
    /// `true`, wenn `may_depend_on` den [`WILDCARD`] enthält.
    any: bool,
    /// Ob Pakete dieser Schicht keine `*-sys`-Crate erreichen dürfen.
    forbid_sys_crates: bool,
}

impl LayerRule {
    /// Ob eine Kante in die Schicht `layer` erlaubt ist.
    fn allows(&self, layer: &str) -> bool {
        self.any || self.may_depend_on.contains(layer)
    }

    /// Die erlaubten Schichten, lesbar für eine Fehlermeldung.
    fn describe(&self) -> String {
        if self.any {
            return WILDCARD.to_owned();
        }
        self.may_depend_on
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Einordnung eines Pakets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageClass {
    /// Schlüssel der Schicht (`F`, `I`, …).
    layer: String,
    /// Ob das Paket zur privilegierten TCB gehört.
    tcb: bool,
}

/// Eine geduldete Kante gegen die Schichtregel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Exception {
    from: String,
    to: String,
    reason: String,
    until: String,
}

/// Die geparste, in sich geprüfte Richtlinie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    rules: BTreeMap<String, LayerRule>,
    packages: BTreeMap<String, PackageClass>,
    exceptions: Vec<Exception>,
    tcb: BTreeMap<String, BTreeSet<String>>,
    sys_forbid_for_tcb: bool,
    /// Erlaubte `*-sys`-Crates mit Begründung.
    sys_allowed: BTreeMap<String, String>,
    /// Externe Crates, die bestimmte Schichten (bzw. die TCB) in ihrer
    /// transitiven Hülle nicht erreichen dürfen (`[[forbidden_crates]]`).
    forbidden: Vec<ForbiddenCrates>,
}

/// Eine Regel aus `[[forbidden_crates]]`: `names` darf von keinem Paket der
/// Schichten `layers` (und, mit `tcb = true`, von keinem TCB-Paket) über
/// `[dependencies]`/`[build-dependencies]` samt ihren `[target.'…']`-Varianten
/// erreicht werden, verglichen nach Paketname (siehe [`ManifestEdges`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForbiddenCrates {
    names: BTreeSet<String>,
    layers: BTreeSet<String>,
    /// Einzelne Pakete, für die die Regel zusätzlich gilt.
    packages: BTreeSet<String>,
    tcb: bool,
    reason: String,
}

impl ForbiddenCrates {
    fn applies_to(&self, name: &str, class: &PackageClass) -> bool {
        self.layers.contains(&class.layer)
            || self.packages.contains(name)
            || (self.tcb && class.tcb)
    }
}

/// Prüft, dass eine Tabelle nur bekannte Schlüssel trägt.
///
/// # Description
/// Ein Tippfehler wie `tbc = true` soll rot werden, nicht still als
/// „kein TCB-Paket" gelesen werden.
fn check_keys(table: &toml::Table, allowed: &[&str], context: &str) -> Result<(), String> {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "{POLICY_PATH}: unbekannter Schlüssel '{key}' in {context} (erlaubt: {})",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

/// Liest einen Wert als Tabelle.
fn as_table<'a>(value: &'a toml::Value, context: &str) -> Result<&'a toml::Table, String> {
    value
        .as_table()
        .ok_or_else(|| format!("{POLICY_PATH}: {context} muss eine Tabelle sein"))
}

/// Liest einen Pflicht-Schlüssel als nicht leere Zeichenkette.
fn required_str(table: &toml::Table, key: &str, context: &str) -> Result<String, String> {
    match table.get(key).and_then(toml::Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_owned()),
        _ => Err(format!(
            "{POLICY_PATH}: {context} braucht '{key}' als nicht leere Zeichenkette"
        )),
    }
}

/// Liest einen optionalen Wahrheitswert (fehlend = `false`).
fn optional_bool(table: &toml::Table, key: &str, context: &str) -> Result<bool, String> {
    match table.get(key) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{POLICY_PATH}: {context}.{key} muss true/false sein")),
    }
}

/// Liest eine Liste von Zeichenketten.
fn string_list(value: &toml::Value, context: &str) -> Result<Vec<String>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("{POLICY_PATH}: {context} muss eine Liste sein"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{POLICY_PATH}: {context} darf nur Zeichenketten enthalten"))
        })
        .collect()
}

/// Liest eine optionale Liste von Zeichenketten als Menge (fehlend = leer).
fn string_set(table: &toml::Table, key: &str, context: &str) -> Result<BTreeSet<String>, String> {
    match table.get(key) {
        None => Ok(BTreeSet::new()),
        Some(value) => Ok(string_list(value, &format!("{context}.{key}"))?
            .into_iter()
            .collect()),
    }
}

impl Policy {
    /// Parst und validiert die Richtlinie.
    ///
    /// # Arguments
    /// - `text` (`&str`): Inhalt von [`POLICY_PATH`].
    ///
    /// # Errors
    /// Bei TOML-Syntaxfehlern, unbekannten Schlüsseln, unbekannten Schichten,
    /// fehlenden Regeln, doppelten Einträgen und TCB-Widersprüchen.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root: toml::Table =
            toml::from_str(text).map_err(|error| format!("{POLICY_PATH}: {error}"))?;
        check_keys(
            &root,
            &[
                "layers",
                "rules",
                "sys_crates",
                "packages",
                "tcb",
                "exceptions",
                "forbidden_crates",
            ],
            "der Wurzel",
        )?;

        let layers = Self::parse_layers(&root)?;
        let rules = Self::parse_rules(&root, &layers)?;
        let packages = Self::parse_packages(&root, &layers)?;
        let tcb = Self::parse_tcb(&root, &packages)?;
        let (sys_forbid_for_tcb, sys_allowed) = Self::parse_sys(&root)?;
        let exceptions = Self::parse_exceptions(&root, &packages)?;
        let forbidden = Self::parse_forbidden(&root, &layers, &packages)?;

        Ok(Self {
            rules,
            packages,
            exceptions,
            tcb,
            sys_forbid_for_tcb,
            sys_allowed,
            forbidden,
        })
    }

    fn parse_forbidden(
        root: &toml::Table,
        layers: &BTreeSet<String>,
        packages: &BTreeMap<String, PackageClass>,
    ) -> Result<Vec<ForbiddenCrates>, String> {
        let mut rules = Vec::new();
        let Some(value) = root.get("forbidden_crates") else {
            return Ok(rules);
        };
        let array = value.as_array().ok_or_else(|| {
            format!("{POLICY_PATH}: [[forbidden_crates]] muss eine Tabellenliste sein")
        })?;
        for item in array {
            let context = "[[forbidden_crates]]";
            let entry = as_table(item, context)?;
            check_keys(
                entry,
                &["names", "layers", "packages", "tcb", "reason"],
                context,
            )?;
            let names = string_set(entry, "names", context)?;
            let rule_layers = string_set(entry, "layers", context)?;
            for layer in &rule_layers {
                if !layers.contains(layer) {
                    return Err(format!(
                        "{POLICY_PATH}: {context} nennt die unbekannte Schicht '{layer}'"
                    ));
                }
            }
            let rule_packages = string_set(entry, "packages", context)?;
            for package in &rule_packages {
                if !packages.contains_key(package) {
                    return Err(format!(
                        "{POLICY_PATH}: {context} nennt das nicht klassifizierte Paket '{package}'"
                    ));
                }
            }
            let tcb = optional_bool(entry, "tcb", context)?;
            if names.is_empty() || (rule_layers.is_empty() && rule_packages.is_empty() && !tcb) {
                return Err(format!(
                    "{POLICY_PATH}: {context} braucht mindestens einen Namen und eine Schicht, ein Paket oder tcb = true"
                ));
            }
            rules.push(ForbiddenCrates {
                names,
                layers: rule_layers,
                packages: rule_packages,
                tcb,
                reason: required_str(entry, "reason", context)?,
            });
        }
        Ok(rules)
    }

    fn parse_layers(root: &toml::Table) -> Result<BTreeSet<String>, String> {
        let table = as_table(
            root.get("layers")
                .ok_or_else(|| format!("{POLICY_PATH}: [layers] fehlt"))?,
            "[layers]",
        )?;
        let mut layers = BTreeSet::new();
        for (key, description) in table {
            if key == WILDCARD {
                return Err(format!("{POLICY_PATH}: '{WILDCARD}' ist kein Schichtname"));
            }
            if description.as_str().is_none() {
                return Err(format!(
                    "{POLICY_PATH}: [layers].{key} muss eine Beschreibung (Zeichenkette) sein"
                ));
            }
            layers.insert(key.clone());
        }
        if layers.is_empty() {
            return Err(format!("{POLICY_PATH}: [layers] ist leer"));
        }
        Ok(layers)
    }

    fn parse_rules(
        root: &toml::Table,
        layers: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, LayerRule>, String> {
        let table = as_table(
            root.get("rules")
                .ok_or_else(|| format!("{POLICY_PATH}: [rules] fehlt"))?,
            "[rules]",
        )?;
        let mut rules = BTreeMap::new();
        for (layer, value) in table {
            let context = format!("[rules].{layer}");
            if !layers.contains(layer) {
                return Err(format!(
                    "{POLICY_PATH}: {context} nennt eine Schicht, die in [layers] fehlt"
                ));
            }
            let rule = as_table(value, &context)?;
            check_keys(rule, &["may_depend_on", "forbid_sys_crates"], &context)?;
            let targets = string_list(
                rule.get("may_depend_on")
                    .ok_or_else(|| format!("{POLICY_PATH}: {context} braucht 'may_depend_on'"))?,
                &format!("{context}.may_depend_on"),
            )?;
            let mut may_depend_on = BTreeSet::new();
            let mut any = false;
            for target in targets {
                if target == WILDCARD {
                    any = true;
                } else if layers.contains(&target) {
                    may_depend_on.insert(target);
                } else {
                    return Err(format!(
                        "{POLICY_PATH}: {context}.may_depend_on nennt die unbekannte Schicht '{target}'"
                    ));
                }
            }
            let forbid_sys_crates = optional_bool(rule, "forbid_sys_crates", &context)?;
            rules.insert(
                layer.clone(),
                LayerRule {
                    may_depend_on,
                    any,
                    forbid_sys_crates,
                },
            );
        }
        for layer in layers {
            if !rules.contains_key(layer) {
                return Err(format!(
                    "{POLICY_PATH}: Schicht '{layer}' hat keine Regel unter [rules]"
                ));
            }
        }
        Ok(rules)
    }

    fn parse_packages(
        root: &toml::Table,
        layers: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, PackageClass>, String> {
        let table = as_table(
            root.get("packages")
                .ok_or_else(|| format!("{POLICY_PATH}: [packages] fehlt"))?,
            "[packages]",
        )?;
        let mut packages = BTreeMap::new();
        for (name, value) in table {
            let context = format!("[packages.\"{name}\"]");
            let entry = as_table(value, &context)?;
            check_keys(entry, &["layer", "tcb"], &context)?;
            let layer = required_str(entry, "layer", &context)?;
            if !layers.contains(&layer) {
                return Err(format!(
                    "{POLICY_PATH}: {context} nennt die unbekannte Schicht '{layer}'"
                ));
            }
            let tcb = optional_bool(entry, "tcb", &context)?;
            packages.insert(name.clone(), PackageClass { layer, tcb });
        }
        Ok(packages)
    }

    fn parse_tcb(
        root: &toml::Table,
        packages: &BTreeMap<String, PackageClass>,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
        let mut tcb = BTreeMap::new();
        if let Some(value) = root.get("tcb") {
            for (name, entry) in as_table(value, "[tcb]")? {
                let context = format!("[tcb.\"{name}\"]");
                if !packages.get(name).is_some_and(|class| class.tcb) {
                    return Err(format!(
                        "{POLICY_PATH}: {context} gehört zu keinem Paket mit 'tcb = true'"
                    ));
                }
                let entry = as_table(entry, &context)?;
                check_keys(entry, &["allowed_internal"], &context)?;
                let allowed = string_list(
                    entry.get("allowed_internal").ok_or_else(|| {
                        format!("{POLICY_PATH}: {context} braucht 'allowed_internal'")
                    })?,
                    &format!("{context}.allowed_internal"),
                )?;
                let mut set = BTreeSet::new();
                for dep in allowed {
                    if !packages.contains_key(&dep) {
                        return Err(format!(
                            "{POLICY_PATH}: {context}.allowed_internal nennt das nicht klassifizierte Paket '{dep}'"
                        ));
                    }
                    if !set.insert(dep.clone()) {
                        return Err(format!(
                            "{POLICY_PATH}: {context}.allowed_internal nennt '{dep}' doppelt"
                        ));
                    }
                }
                tcb.insert(name.clone(), set);
            }
        }
        for (name, class) in packages {
            if class.tcb && !tcb.contains_key(name) {
                return Err(format!(
                    "{POLICY_PATH}: '{name}' ist 'tcb = true', aber [tcb.\"{name}\"] fehlt — ein TCB-Paket braucht eine Allowlist (§23)"
                ));
            }
        }
        Ok(tcb)
    }

    fn parse_sys(root: &toml::Table) -> Result<(bool, BTreeMap<String, String>), String> {
        let mut allowed = BTreeMap::new();
        let Some(value) = root.get("sys_crates") else {
            return Ok((false, allowed));
        };
        let table = as_table(value, "[sys_crates]")?;
        check_keys(table, &["forbid_for_tcb", "allow"], "[sys_crates]")?;
        let forbid_for_tcb = optional_bool(table, "forbid_for_tcb", "[sys_crates]")?;
        if let Some(list) = table.get("allow") {
            let array = list.as_array().ok_or_else(|| {
                format!("{POLICY_PATH}: [[sys_crates.allow]] muss eine Tabellenliste sein")
            })?;
            for item in array {
                let entry = as_table(item, "[[sys_crates.allow]]")?;
                check_keys(entry, &["name", "reason"], "[[sys_crates.allow]]")?;
                let name = required_str(entry, "name", "[[sys_crates.allow]]")?;
                let reason = required_str(entry, "reason", "[[sys_crates.allow]]")?;
                if allowed.insert(name.clone(), reason).is_some() {
                    return Err(format!(
                        "{POLICY_PATH}: [[sys_crates.allow]] nennt '{name}' doppelt"
                    ));
                }
            }
        }
        Ok((forbid_for_tcb, allowed))
    }

    fn parse_exceptions(
        root: &toml::Table,
        packages: &BTreeMap<String, PackageClass>,
    ) -> Result<Vec<Exception>, String> {
        let mut exceptions: Vec<Exception> = Vec::new();
        let Some(value) = root.get("exceptions") else {
            return Ok(exceptions);
        };
        let array = value
            .as_array()
            .ok_or_else(|| format!("{POLICY_PATH}: [[exceptions]] muss eine Tabellenliste sein"))?;
        for item in array {
            let entry = as_table(item, "[[exceptions]]")?;
            check_keys(entry, &["from", "to", "reason", "until"], "[[exceptions]]")?;
            let exception = Exception {
                from: required_str(entry, "from", "[[exceptions]]")?,
                to: required_str(entry, "to", "[[exceptions]]")?,
                reason: required_str(entry, "reason", "[[exceptions]]")?,
                until: required_str(entry, "until", "[[exceptions]]")?,
            };
            for name in [&exception.from, &exception.to] {
                if !packages.contains_key(name) {
                    return Err(format!(
                        "{POLICY_PATH}: Ausnahme {} → {} nennt das nicht klassifizierte Paket '{name}'",
                        exception.from, exception.to
                    ));
                }
            }
            if exceptions
                .iter()
                .any(|seen| seen.from == exception.from && seen.to == exception.to)
            {
                return Err(format!(
                    "{POLICY_PATH}: Ausnahme {} → {} steht doppelt",
                    exception.from, exception.to
                ));
            }
            exceptions.push(exception);
        }
        Ok(exceptions)
    }

    /// Ob für dieses Paket das `*-sys`-Verbot gilt.
    fn forbids_sys(&self, class: &PackageClass) -> bool {
        (class.tcb && self.sys_forbid_for_tcb)
            || self
                .rules
                .get(&class.layer)
                .is_some_and(|rule| rule.forbid_sys_crates)
    }
}

/// Die Abhängigkeitskanten aus `Cargo.lock`, je Crate-Name.
///
/// # Description
/// Kommt ein Name in mehreren Fassungen vor, werden die Kanten aller
/// Fassungen vereinigt — eine Überschätzung, die keine Fassung übersieht
/// (siehe Moduldoku).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockIndex {
    deps: HashMap<String, BTreeSet<String>>,
}

impl LockIndex {
    /// Parst den Inhalt eines `Cargo.lock`.
    ///
    /// # Errors
    /// Bei einem TOML-Fehler oder einem `[[package]]` ohne Namen.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root: toml::Table =
            toml::from_str(text).map_err(|error| format!("Cargo.lock: {error}"))?;
        let mut deps: HashMap<String, BTreeSet<String>> = HashMap::new();
        let Some(packages) = root.get("package").and_then(toml::Value::as_array) else {
            return Ok(Self { deps });
        };
        for package in packages {
            let name = package
                .get("name")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| "Cargo.lock: [[package]] ohne 'name'".to_owned())?;
            let entry = deps.entry(name.to_owned()).or_default();
            if let Some(list) = package.get("dependencies").and_then(toml::Value::as_array) {
                for item in list.iter().filter_map(toml::Value::as_str) {
                    // Cargo hängt die Fassung nur an, wenn der Name allein
                    // mehrdeutig wäre: "toml 1.1.3+spec-1.1.0".
                    let dep = item.split_once(' ').map_or(item, |(dep, _)| dep);
                    entry.insert(dep.to_owned());
                }
            }
        }
        Ok(Self { deps })
    }

    /// Die Abhängigkeiten von `name` laut `Cargo.lock` (alle Fassungen
    /// vereinigt), oder `None`, wenn `name` dort nicht verzeichnet ist.
    /// Genutzt von den Hüllenregeln in `gate_edges.rs`.
    #[must_use]
    pub fn deps_of(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.deps.get(name)
    }

    /// Liest `<repo_root>/Cargo.lock`.
    ///
    /// # Errors
    /// Wenn die Datei fehlt oder nicht lesbar ist — ohne Lockfile ist keine
    /// `*-sys`-Hülle zu bilden, das ist kein „nichts zu prüfen".
    pub fn load(repo_root: &Path) -> Result<Self, String> {
        let path = repo_root.join("Cargo.lock");
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("'{}' nicht lesbar: {error}", path.display()))?;
        Self::parse(&text)
    }
}

/// Die Abhängigkeitstabellen, deren Kanten in ein ausgeliefertes Artefakt
/// gelangen: normale und Build-Abhängigkeiten, auch in der alten
/// Unterstrich-Schreibweise, die Cargo weiterhin liest. Dev-Tabellen fehlen
/// bewusst; jede gilt zusätzlich unter `[target.'…']`.
const SHIPPED_DEPENDENCY_TABLES: [&str; 3] =
    ["dependencies", "build-dependencies", "build_dependencies"];

/// Die aufgelösten, ausgelieferten Abhängigkeiten jedes lokalen Pakets, aus
/// den Manifesten gelesen — die Kanten der `[[forbidden_crates]]`-Hülle.
///
/// # Description
/// Je Paket die Paketnamen aller Einträge in [`SHIPPED_DEPENDENCY_TABLES`]
/// und deren `[target.'…']`-Varianten. Ein Eintrag zählt unter seiner
/// Paketidentität, nicht unter seinem Tabellenschlüssel:
///
/// - ohne `workspace = true`: `package = "…"`, sonst der Schlüssel;
/// - mit `workspace = true`: `package` des gleichnamigen Eintrags in
///   `[workspace.dependencies]` der Wurzel, sonst der Schlüssel; ein lokales
///   `package` daneben (das Cargo nicht auswertet) zählt konservativ mit.
///
/// Siehe Moduldoku, Abschnitt „Wie die `[[forbidden_crates]]`-Hülle
/// entsteht".
///
/// # Concurrency
/// Ein reiner Wert ohne innere Veränderlichkeit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestEdges {
    deps: HashMap<String, BTreeSet<String>>,
}

impl ManifestEdges {
    /// Baut die Kanten aus bereits geparsten Manifesten.
    ///
    /// # Arguments
    /// - `root` (`&toml::Table`): die Wurzel-`Cargo.toml`; liefert
    ///   `[workspace.dependencies]` und zählt selbst als Paket, wenn sie
    ///   `[package].name` trägt und nicht schon in `packages` steht.
    /// - `packages` (`&[(String, toml::Table)]`): je lokalem Paket
    ///   `(Paketname, Manifest)`, wie
    ///   [`super::operator_decision::load_manifests`] sie liefert. Mehrere
    ///   Manifeste desselben Namens werden vereinigt (Überschätzung).
    ///
    /// # Returns
    /// Die Kanten je Paketname.
    #[must_use]
    pub fn from_manifests(root: &toml::Table, packages: &[(String, toml::Table)]) -> Self {
        let workspace = root
            .get("workspace")
            .and_then(toml::Value::as_table)
            .and_then(|section| section.get("dependencies"))
            .and_then(toml::Value::as_table);
        let root_package = root
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .filter(|name| !packages.iter().any(|(known, _)| known.as_str() == *name))
            .map(|name| (name, root));
        let mut deps: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (name, manifest) in packages
            .iter()
            .map(|(name, manifest)| (name.as_str(), manifest))
            .chain(root_package)
        {
            let entry = deps.entry(name.to_owned()).or_default();
            for table in shipped_dependency_tables(manifest) {
                for (key, value) in table {
                    entry.extend(resolved_packages(key, value, workspace));
                }
            }
        }
        Self { deps }
    }

    /// Liest die Manifeste unterhalb von `repo_root` (dieselbe Eingabemenge
    /// wie das Gate `operator-decision`) und baut daraus die Kanten.
    ///
    /// # Errors
    /// Wie [`super::operator_decision::load_manifests`]: ein unlesbarer
    /// Workspace, ein unlesbares oder ungültiges Manifest oder ein fehlendes
    /// Ziel einer `path`-Abhängigkeit.
    pub fn load(repo_root: &Path) -> Result<Self, String> {
        let (root, packages) = super::operator_decision::load_manifests(repo_root)?;
        Ok(Self::from_manifests(&root, &packages))
    }

    /// Die aufgelösten Abhängigkeiten von `name`, oder `None`, wenn kein
    /// Manifest dieses Namens gelesen wurde.
    #[must_use]
    pub fn deps_of(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.deps.get(name)
    }
}

/// Die Tabellen aus [`SHIPPED_DEPENDENCY_TABLES`] eines Manifests, auf
/// oberster Ebene und unter jedem `[target.'…']`.
fn shipped_dependency_tables(manifest: &toml::Table) -> Vec<&toml::Table> {
    let targets = manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|table| table.values())
        .filter_map(toml::Value::as_table);
    std::iter::once(manifest)
        .chain(targets)
        .flat_map(|section| {
            SHIPPED_DEPENDENCY_TABLES
                .iter()
                .filter_map(move |kind| section.get(*kind).and_then(toml::Value::as_table))
        })
        .collect()
}

/// Die Paketnamen, unter denen ein Abhängigkeitseintrag `key = value` zählt
/// (siehe [`ManifestEdges`]).
fn resolved_packages(
    key: &str,
    value: &toml::Value,
    workspace: Option<&toml::Table>,
) -> Vec<String> {
    let entry = value.as_table();
    let local = entry
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_str);
    let inherits = entry
        .and_then(|table| table.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true);
    if !inherits {
        return vec![local.unwrap_or(key).to_owned()];
    }
    let inherited = workspace
        .and_then(|dependencies| dependencies.get(key))
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_str)
        .unwrap_or(key);
    let mut names = vec![inherited.to_owned()];
    if let Some(local) = local.filter(|local| *local != inherited) {
        names.push(local.to_owned());
    }
    names
}

/// Führt das Gate `arch` aus.
///
/// # Errors
/// Wenn Graph, Richtlinie, `Cargo.lock` oder ein Manifest nicht lesbar sind
/// oder die Richtlinie einen Formfehler hat. Ein **Verstoß** ist kein `Err`,
/// sondern steht im Bericht.
pub fn run() -> Result<GateReport, String> {
    let repo_root = Path::new(".");
    let graph = super::load_all_workspaces(repo_root)?;
    let policy = load_policy(repo_root)?;
    let lock = LockIndex::load(repo_root)?;
    let manifests = ManifestEdges::load(repo_root)?;
    Ok(evaluate_with_manifests(&graph, &policy, &lock, &manifests))
}

/// Liest und parst [`POLICY_PATH`] unterhalb von `repo_root`.
///
/// # Errors
/// Wenn die Datei nicht lesbar ist oder [`Policy::parse`] scheitert.
pub fn load_policy(repo_root: &Path) -> Result<Policy, String> {
    let path = repo_root.join(POLICY_PATH);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("'{}' nicht lesbar: {error}", path.display()))?;
    Policy::parse(&text)
}

/// Prüft einen geladenen Graphen gegen die Richtlinie, ohne gelesene
/// Manifeste — nur für Tests.
///
/// # Description
/// Wie [`evaluate_with_manifests`] mit leeren [`ManifestEdges`]: die
/// `[[forbidden_crates]]`-Hülle folgt dann den Schlüsseln des Graphen. Ein
/// umbenannter Schlüssel ist dort ein unbekannter Name und damit rot (fail
/// closed); Target-Tabellen sieht diese Fassung nicht. [`run`] nutzt deshalb
/// [`evaluate_with_manifests`]; diese Kurzform existiert nur unter
/// `cfg(test)`, damit der Binär-Build keinen toten Code trägt.
///
/// # Returns
/// Den Bericht, wie [`evaluate_with_manifests`].
#[cfg(test)]
#[must_use]
fn evaluate(graph: &WorkspaceGraph, policy: &Policy, lock: &LockIndex) -> GateReport {
    evaluate_with_manifests(graph, policy, lock, &ManifestEdges::default())
}

/// Prüft einen geladenen Graphen gegen die Richtlinie.
///
/// # Description
/// Getrennt von [`run`], damit jede Regel gegen einen **konstruierten**
/// Graphen samt Lockfile und Manifesten prüfbar ist, einschließlich ihres
/// roten Zweigs. Für die `[[forbidden_crates]]`-Hülle folgt ein lokales
/// Paket den Kanten aus `manifests`, falls dort verzeichnet, sonst denen des
/// Graphen; Schichtregel, TCB-Allowlisten und `*-sys`-Hülle folgen immer dem
/// Graphen.
///
/// # Arguments
/// - `graph` (`&WorkspaceGraph`): der Abhängigkeitsgraph.
/// - `policy` (`&Policy`): die geparste Richtlinie.
/// - `lock` (`&LockIndex`): die Kanten externer Crates.
/// - `manifests` (`&ManifestEdges`): die aufgelösten Kanten lokaler Pakete.
///
/// # Returns
/// Den Bericht; `checked` zählt klassifizierte Pakete, interne Kanten, die
/// Einträge jeder geprüften `*-sys`-Hülle und jede geprüfte
/// `[[forbidden_crates]]`-Hülle.
#[must_use]
pub fn evaluate_with_manifests(
    graph: &WorkspaceGraph,
    policy: &Policy,
    lock: &LockIndex,
    manifests: &ManifestEdges,
) -> GateReport {
    let mut checked = 0usize;
    let mut violations = Vec::new();
    let mut used_exceptions: HashSet<(&str, &str)> = HashSet::new();
    let mut used_tcb: HashSet<(&str, &str)> = HashSet::new();

    for node in &graph.crates {
        checked += 1;
        if !policy.packages.contains_key(&node.name) {
            violations.push(format!(
                "{}: nicht in {POLICY_PATH} klassifiziert — jedes Paket braucht eine Schicht unter [packages] (§47)",
                node.name
            ));
        }
    }

    for node in &graph.crates {
        let from_class = policy.packages.get(&node.name);
        for dep in &node.deps {
            checked += 1;
            let (Some(from_class), Some(to_class)) = (from_class, policy.packages.get(dep)) else {
                // Das nicht klassifizierte Paket ist oben schon gemeldet.
                continue;
            };
            let allowed = policy
                .rules
                .get(&from_class.layer)
                .is_some_and(|rule| rule.allows(&to_class.layer));
            let exception = policy
                .exceptions
                .iter()
                .find(|exception| exception.from == node.name && exception.to == *dep);
            if let Some(exception) = exception {
                used_exceptions.insert((exception.from.as_str(), exception.to.as_str()));
                if allowed {
                    violations.push(format!(
                        "Ausnahme {} → {} ist veraltet: die Kante ist nach [rules] ohnehin erlaubt — Eintrag streichen (Ratsche; bis {}: {})",
                        exception.from, exception.to, exception.until, exception.reason
                    ));
                }
            } else if !allowed {
                let permitted = policy
                    .rules
                    .get(&from_class.layer)
                    .map(LayerRule::describe)
                    .unwrap_or_default();
                violations.push(format!(
                    "{} ({}) → {dep} ({}) verletzt die Schichtregel: {} darf nur auf [{permitted}] zeigen (§6)",
                    node.name, from_class.layer, to_class.layer, from_class.layer
                ));
            }

            if from_class.tcb {
                let allowlist = policy.tcb.get(&node.name);
                if let Some(entry) = allowlist.and_then(|set| set.get(dep)) {
                    used_tcb.insert((node.name.as_str(), entry.as_str()));
                } else {
                    violations.push(format!(
                        "{} (TCB) → {dep}: nicht in [tcb.\"{}\"].allowed_internal — neue interne Abhängigkeit eines TCB-Pakets (§23)",
                        node.name, node.name
                    ));
                }
            }
        }
    }

    for exception in &policy.exceptions {
        if !used_exceptions.contains(&(exception.from.as_str(), exception.to.as_str())) {
            violations.push(format!(
                "Ausnahme {} → {} ist veraltet: die Kante existiert nicht (mehr) — Eintrag streichen (Ratsche; bis {}: {})",
                exception.from, exception.to, exception.until, exception.reason
            ));
        }
    }

    let by_name: HashMap<&str, &CrateNode> = graph
        .crates
        .iter()
        .map(|node| (node.name.as_str(), node))
        .collect();

    for (package, allowlist) in &policy.tcb {
        // Angekündigte TCB-Pakete, die es noch nicht gibt, haben keine Kanten.
        if !by_name.contains_key(package.as_str()) {
            continue;
        }
        for dep in allowlist {
            if !used_tcb.contains(&(package.as_str(), dep.as_str())) {
                violations.push(format!(
                    "[tcb.\"{package}\"].allowed_internal nennt '{dep}', aber {package} hängt nicht (mehr) davon ab — Eintrag streichen (Ratsche)"
                ));
            }
        }
    }

    let mut used_sys: HashSet<&str> = HashSet::new();
    for node in &graph.crates {
        let Some(class) = policy.packages.get(&node.name) else {
            continue;
        };
        if !policy.forbids_sys(class) {
            continue;
        }
        let hull = sys_hull(node, &by_name, lock);
        checked += hull.visited;
        for missing in &hull.unresolved {
            violations.push(format!(
                "{}: '{missing}' hat keinen Eintrag in Cargo.lock — die *-sys-Hülle wäre unvollständig",
                node.name
            ));
        }
        for (sys, path) in &hull.sys_crates {
            if sys.as_str() == CC {
                // `cc` ist keine Allowlist-Ausnahme, sondern eine fest
                // codierte: erlaubt nur, wenn *jeder* Pfad zu `cc` über
                // `blake3` läuft (siehe CC_EXCEPTION_VIA-Doku). Dazu wird
                // die Hülle ohne diese Zwischenstation neu gebildet — bleibt
                // `cc` darin erreichbar, führt ein Pfad an ihr vorbei.
                let bypass = sys_hull_excluding(node, &by_name, lock, CC_EXCEPTION_VIA);
                if let Some(bypass_path) = bypass.sys_crates.get(sys) {
                    violations.push(format!(
                        "{} erreicht die C-Bauhelfer-Crate '{sys}' über {} — nicht über '{CC_EXCEPTION_VIA}', die einzig begründete Ausnahme (Tier C/A, dependency-review.md)",
                        node.name,
                        bypass_path.join(" → ")
                    ));
                }
                continue;
            }
            if let Some((name, _)) = policy.sys_allowed.get_key_value(sys) {
                used_sys.insert(name.as_str());
            } else {
                let scope = if class.tcb {
                    format!("Schicht {} / TCB", class.layer)
                } else {
                    format!("Schicht {}", class.layer)
                };
                let kind = if sys.ends_with(SYS_SUFFIX) {
                    "-sys-Crate"
                } else {
                    "C-Bauhelfer-Crate"
                };
                violations.push(format!(
                    "{} erreicht die {kind} '{sys}' über {} — in {scope} verboten, außer mit Begründung unter [[sys_crates.allow]] (§40)",
                    node.name,
                    path.join(" → ")
                ));
            }
        }
    }
    // Je (Paket, Name) nur eine Meldung, auch wenn mehrere Zeilen gelten.
    let mut unresolved_forbidden: BTreeSet<(&str, String)> = BTreeSet::new();
    for rule in &policy.forbidden {
        for node in &graph.crates {
            let Some(class) = policy.packages.get(&node.name) else {
                continue;
            };
            if !rule.applies_to(&node.name, class) {
                continue;
            }
            let hull = hull_matching(node, &by_name, lock, None, Some(manifests), &|name| {
                rule.names.contains(name)
            });
            checked += 1;
            for missing in hull.unresolved {
                unresolved_forbidden.insert((node.name.as_str(), missing));
            }
            for (name, path) in &hull.sys_crates {
                violations.push(format!(
                    "{} erreicht die verbotene Crate '{name}' über {} — [[forbidden_crates]]: {}",
                    node.name,
                    path.join(" → "),
                    rule.reason
                ));
            }
        }
    }
    for (package, missing) in &unresolved_forbidden {
        violations.push(format!(
            "{package}: '{missing}' ist weder lokales Paket noch in Cargo.lock — die [[forbidden_crates]]-Hülle wäre unvollständig (fail closed)"
        ));
    }

    for name in policy.sys_allowed.keys() {
        if !used_sys.contains(name.as_str()) {
            violations.push(format!(
                "[[sys_crates.allow]] '{name}' wird von keinem geprüften Paket erreicht — Eintrag streichen (Ratsche)"
            ));
        }
    }

    GateReport {
        name: "arch",
        checked,
        violations,
    }
}

/// Ergebnis einer `*-sys`-Hüllenbildung.
#[derive(Debug, Default)]
struct SysHull {
    /// Wie viele Namen die Hülle umfasst (ohne die Wurzel).
    visited: usize,
    /// Gefundene `*-sys`-Crates, je mit dem kürzesten Pfad ab der Wurzel.
    sys_crates: BTreeMap<String, Vec<String>>,
    /// Externe Namen ohne `Cargo.lock`-Eintrag.
    unresolved: BTreeSet<String>,
}

/// Bildet die transitive Hülle von `root` per Breitensuche.
///
/// # Description
/// Interne Knoten folgen ihren `[dependencies]` (intern und extern) und
/// ihren `[build-dependencies]` aus dem Graphen, externe Knoten den Kanten
/// in `Cargo.lock`.
/// Die Breitensuche liefert zu jedem Fund den kürzesten Pfad, damit die
/// Meldung zeigt, **über wen** die Crate hereinkommt.
fn sys_hull(root: &CrateNode, by_name: &HashMap<&str, &CrateNode>, lock: &LockIndex) -> SysHull {
    sys_hull_impl(root, by_name, lock, None)
}

/// Wie [`sys_hull`], aber tritt nicht durch `blocked`: dessen Kanten werden
/// nicht weiterverfolgt, `blocked` selbst bleibt ein Sackgassen-Knoten.
///
/// # Description
/// Dient der `cc`-Ausnahme in [`evaluate_with_manifests`]: bleibt eine
/// gesuchte Crate auch ohne die Zwischenstation [`CC_EXCEPTION_VIA`]
/// erreichbar, führt mindestens ein Pfad an ihr vorbei — die Ausnahme gilt
/// dann nicht.
fn sys_hull_excluding(
    root: &CrateNode,
    by_name: &HashMap<&str, &CrateNode>,
    lock: &LockIndex,
    blocked: &str,
) -> SysHull {
    sys_hull_impl(root, by_name, lock, Some(blocked))
}

fn sys_hull_impl(
    root: &CrateNode,
    by_name: &HashMap<&str, &CrateNode>,
    lock: &LockIndex,
    blocked: Option<&str>,
) -> SysHull {
    hull_matching(root, by_name, lock, blocked, None, &is_native_build_crate)
}

/// Breitensuche wie [`sys_hull`]; gemeldet wird jeder erreichte Name, für den
/// `matches` gilt (im Feld `sys_crates`, mit kürzestem Pfad).
///
/// # Description
/// Mit `manifests` folgt ein dort verzeichnetes lokales Paket seinen
/// aufgelösten Manifest-Kanten (Paketnamen, auch aus `[target.'…']`), sonst
/// den Kanten des Graphen; ab einer externen Crate gilt `Cargo.lock`.
fn hull_matching(
    root: &CrateNode,
    by_name: &HashMap<&str, &CrateNode>,
    lock: &LockIndex,
    blocked: Option<&str>,
    manifests: Option<&ManifestEdges>,
    matches: &dyn Fn(&str) -> bool,
) -> SysHull {
    let mut hull = SysHull::default();
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::from([root.name.clone()]);
    let mut queue: VecDeque<String> = VecDeque::from([root.name.clone()]);

    while let Some(current) = queue.pop_front() {
        if blocked.is_some_and(|blocked| blocked == current) {
            // Nicht durch die blockierte Zwischenstation treten.
            continue;
        }
        let resolved = manifests.and_then(|edges| edges.deps_of(current.as_str()));
        let children: Vec<String> = if let Some(resolved) = resolved {
            resolved.iter().cloned().collect()
        } else if let Some(node) = by_name.get(current.as_str()) {
            // `build_deps` mischt interne und externe Namen; die Unterscheidung
            // trifft `by_name` beim nächsten Schritt, Unbekanntes landet in
            // `unresolved`.
            node.deps
                .iter()
                .chain(node.external_deps.iter())
                .chain(node.build_deps.iter())
                .cloned()
                .collect()
        } else if let Some(deps) = lock.deps.get(&current) {
            deps.iter().cloned().collect()
        } else {
            hull.unresolved.insert(current.clone());
            Vec::new()
        };
        for child in children {
            if seen.insert(child.clone()) {
                parent.insert(child.clone(), current.clone());
                queue.push_back(child);
            }
        }
    }

    hull.visited = seen.len().saturating_sub(1);
    for name in &seen {
        if matches(name) && *name != root.name {
            let mut path = vec![name.clone()];
            let mut cursor = name;
            while let Some(previous) = parent.get(cursor) {
                path.push(previous.clone());
                cursor = previous;
            }
            path.reverse();
            hull.sys_crates.insert(name.clone(), path);
        }
    }
    hull
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::path::PathBuf;

    fn node(name: &str, deps: &[&str], external: &[&str]) -> CrateNode {
        CrateNode {
            name: name.to_owned(),
            version: "0.0.0".to_owned(),
            manifest_path: PathBuf::from(format!("{name}/Cargo.toml")),
            dir: PathBuf::from(name),
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            dev_deps: Vec::new(),
            build_deps: Vec::new(),
            external_deps: external.iter().map(|d| (*d).to_owned()).collect(),
            is_leaf: deps.is_empty(),
            level: 0,
        }
    }

    fn graph(crates: Vec<CrateNode>) -> WorkspaceGraph {
        WorkspaceGraph {
            root: PathBuf::from("."),
            crates,
        }
    }

    /// Grundrichtlinie: vier Schichten, ein TCB-Paket `t`, ein angekündigtes
    /// Paket `future`, das es im Graphen nicht gibt.
    const POLICY: &str = r#"
[layers]
F = "foundation"
I = "infrastructure"
J = "jobs"
A = "application"

[rules]
F.may_depend_on = ["F"]
I.may_depend_on = ["F", "I"]
J.may_depend_on = ["F", "I", "J"]
J.forbid_sys_crates = true
A.may_depend_on = ["*"]

[sys_crates]
forbid_for_tcb = true

[packages."f"]
layer = "F"
[packages."i"]
layer = "I"
[packages."j"]
layer = "J"
[packages."a"]
layer = "A"
[packages."t"]
layer = "I"
tcb = true
[packages."future"]
layer = "J"

[tcb."t"]
allowed_internal = ["f"]
"#;

    const LOCK: &str = r#"
version = 4

[[package]]
name = "tokio"
version = "1.0.0"
dependencies = ["mio"]

[[package]]
name = "mio"
version = "1.0.0"
dependencies = ["libc"]

[[package]]
name = "libc"
version = "0.2.0"
"#;

    /// Der grüne Grundgraph: jede Kante zeigt nach innen.
    fn base_crates() -> Vec<CrateNode> {
        vec![
            node("f", &[], &[]),
            node("i", &["f"], &[]),
            node("j", &["f", "i"], &["tokio"]),
            node("a", &["j", "i", "f"], &[]),
            node("t", &["f"], &[]),
        ]
    }

    /// Der Grundgraph, in dem der gleichnamige Knoten durch `replacement` ersetzt ist.
    fn base_crates_with(replacement: CrateNode) -> Vec<CrateNode> {
        base_crates()
            .into_iter()
            .map(|n| {
                if n.name == replacement.name {
                    replacement.clone()
                } else {
                    n
                }
            })
            .collect()
    }

    fn run_with(crates: Vec<CrateNode>, policy: &str, lock: &str) -> TestResult<GateReport> {
        let policy = Policy::parse(policy).map_err(TestError::Unexpected)?;
        let lock = LockIndex::parse(lock).map_err(TestError::Unexpected)?;
        Ok(evaluate(&graph(crates), &policy, &lock))
    }

    fn with_exception(from: &str, to: &str) -> String {
        format!(
            "{POLICY}\n[[exceptions]]\nfrom = \"{from}\"\nto = \"{to}\"\nreason = \"Test\"\nuntil = \"R11 W2\"\n"
        )
    }

    #[test]
    fn test_evaluate_inward_edges_are_green_and_counted() -> TestResult {
        let report = run_with(base_crates(), POLICY, LOCK)?;

        assert!(report.is_green(), "{:?}", report.violations);
        assert!(
            report.checked > 0,
            "ein Gate ohne Zählung kann 'nichts verletzt' nicht von 'nichts geprüft' unterscheiden"
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_listed_but_absent_package_is_not_a_violation() -> TestResult {
        // `future` steht in der Richtlinie, aber nicht im Graphen.
        let report = run_with(base_crates(), POLICY, LOCK)?;

        assert!(
            !report.violations.iter().any(|v| v.contains("future")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_inversion_edge_is_a_violation() -> TestResult {
        let mut crates = base_crates();
        crates[0] = node("f", &["a"], &[]);

        let report = run_with(crates, POLICY, LOCK)?;

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("f (F) → a (A)") && v.contains("[F]")),
            "die Meldung muss Paar, Schichten und erlaubte Ziele nennen: {:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_exception_suppresses_inversion() -> TestResult {
        let mut crates = base_crates();
        crates[1] = node("i", &["f", "j"], &[]);

        let without = run_with(crates.clone(), POLICY, LOCK)?;
        let with = run_with(crates, &with_exception("i", "j"), LOCK)?;

        assert!(!without.is_green(), "ohne Ausnahme muss I → J rot sein");
        assert!(with.is_green(), "{:?}", with.violations);
        Ok(())
    }

    #[test]
    fn test_evaluate_exception_without_edge_is_stale() -> TestResult {
        let report = run_with(base_crates(), &with_exception("i", "a"), LOCK)?;

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("Ausnahme i → a") && v.contains("existiert nicht")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_exception_for_allowed_edge_is_stale() -> TestResult {
        // j → i ist nach den Regeln erlaubt; die Ausnahme ist überflüssig.
        let report = run_with(base_crates(), &with_exception("j", "i"), LOCK)?;

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("Ausnahme j → i") && v.contains("ohnehin erlaubt")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_unclassified_package_is_a_violation() -> TestResult {
        let mut crates = base_crates();
        crates.push(node("neu", &["f"], &[]));

        let report = run_with(crates, POLICY, LOCK)?;

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.starts_with("neu: nicht in")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_new_tcb_dependency_is_a_violation() -> TestResult {
        // t → i ist nach der Schichtregel (I → I) erlaubt, aber nicht auf
        // der TCB-Allowlist.
        let mut crates = base_crates();
        crates[4] = node("t", &["f", "i"], &[]);

        let report = run_with(crates, POLICY, LOCK)?;

        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("t (TCB) → i") && v.contains("allowed_internal")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_unused_tcb_allowlist_entry_is_stale() -> TestResult {
        let mut crates = base_crates();
        crates[4] = node("t", &[], &[]);

        let report = run_with(crates, POLICY, LOCK)?;

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("[tcb.\"t\"]") && v.contains("'f'")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_sys_crate_in_jobs_hull_is_a_violation() -> TestResult {
        let lock = LOCK.replace(
            "dependencies = [\"libc\"]",
            "dependencies = [\"libc\", \"openssl-sys\"]",
        ) + "\n[[package]]\nname = \"openssl-sys\"\nversion = \"0.9.0\"\n";

        let report = run_with(base_crates(), POLICY, &lock)?;

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("'openssl-sys'") && v.contains("j → tokio → mio → openssl-sys")),
            "die Meldung muss den Pfad nennen: {:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_sys_crate_outside_forbidden_layers_is_ignored() -> TestResult {
        // `a` (Schicht A) erreicht eine -sys-Crate; A hat kein Verbot.
        let mut crates = base_crates();
        crates[3] = node("a", &["j", "i", "f"], &["zstd-sys"]);
        let lock = format!("{LOCK}\n[[package]]\nname = \"zstd-sys\"\nversion = \"2.0.0\"\n");

        let report = run_with(crates, POLICY, &lock)?;

        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    #[test]
    fn test_evaluate_sys_crate_in_tcb_hull_is_a_violation() -> TestResult {
        let mut crates = base_crates();
        crates[4] = node("t", &["f"], &["zstd-sys"]);
        let lock = format!("{LOCK}\n[[package]]\nname = \"zstd-sys\"\nversion = \"2.0.0\"\n");

        let report = run_with(crates, POLICY, &lock)?;

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("t erreicht") && v.contains("TCB")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_allowlisted_sys_crate_is_green_and_unused_entry_is_stale() -> TestResult {
        let lock = LOCK.replace(
            "dependencies = [\"libc\"]",
            "dependencies = [\"libc\", \"linux-raw-sys\"]",
        ) + "\n[[package]]\nname = \"linux-raw-sys\"\nversion = \"0.11.0\"\n";
        let allow = |name: &str| {
            format!("{POLICY}\n[[sys_crates.allow]]\nname = \"{name}\"\nreason = \"reines Rust\"\n")
        };

        let used = run_with(base_crates(), &allow("linux-raw-sys"), &lock)?;
        let unused = run_with(base_crates(), &allow("windows-sys"), LOCK)?;

        assert!(used.is_green(), "{:?}", used.violations);
        assert!(
            unused
                .violations
                .iter()
                .any(|v| v.contains("'windows-sys'") && v.contains("Ratsche")),
            "{:?}",
            unused.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_c_build_helper_in_jobs_hull_is_a_violation() -> TestResult {
        // `ring` trägt keinen `-sys`-Namen, ist aber ein C_BUILD_HELPERS-
        // Eintrag; muss wie eine `-sys`-Crate in der J-Hülle rot werden.
        let lock = LOCK.replace(
            "dependencies = [\"libc\"]",
            "dependencies = [\"libc\", \"ring\"]",
        ) + "\n[[package]]\nname = \"ring\"\nversion = \"0.17.0\"\n";

        let report = run_with(base_crates(), POLICY, &lock)?;

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("'ring'") && v.contains("j → tokio → mio → ring")),
            "die Meldung muss den Pfad nennen: {:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_cc_only_via_blake3_is_green() -> TestResult {
        // `j` erreicht `cc` ausschließlich über `blake3` — die fest codierte
        // Ausnahme greift, kein Verstoß.
        let mut crates = base_crates();
        crates[2] = node("j", &["f", "i"], &["tokio", "blake3"]);
        let lock = format!(
            "{LOCK}\n[[package]]\nname = \"blake3\"\nversion = \"1.0.0\"\ndependencies = [\"cc\"]\n\n[[package]]\nname = \"cc\"\nversion = \"1.0.0\"\n"
        );

        let report = run_with(crates, POLICY, &lock)?;

        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    #[test]
    fn test_evaluate_cc_via_other_path_is_a_violation() -> TestResult {
        // `j` erreicht `cc` über `tokio`, nicht über `blake3` — die Ausnahme
        // gilt nicht (kein Blanket-Allow von `cc`).
        let lock = LOCK.replace(
            "dependencies = [\"mio\"]",
            "dependencies = [\"mio\", \"cc\"]",
        ) + "\n[[package]]\nname = \"cc\"\nversion = \"1.0.0\"\n";

        let report = run_with(base_crates(), POLICY, &lock)?;

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("'cc'") && v.contains("j → tokio → cc")),
            "die Meldung muss den Bypass-Pfad ohne blake3 nennen: {:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_c_build_helper_as_build_dependency_is_a_violation() -> TestResult {
        // `j` zieht `cc` direkt als `[build-dependencies]` herein, nicht über
        // `blake3` — die Hülle muss der Build-Kante folgen, und die
        // `cc`-Ausnahme greift nicht.
        let mut j = node("j", &["f", "i"], &["tokio"]);
        j.build_deps = vec!["cc".to_owned()];
        let lock = format!("{LOCK}\n[[package]]\nname = \"cc\"\nversion = \"1.0.0\"\n");

        let report = run_with(base_crates_with(j), POLICY, &lock)?;

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("'cc'") && v.contains("j → cc")),
            "die Meldung muss den Bypass-Pfad über die Build-Kante nennen: {:?}",
            report.violations
        );
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.contains("keinen Eintrag in Cargo.lock")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_sys_crate_as_build_dependency_of_hull_member_is_a_violation() -> TestResult {
        // `i` (Schicht I) darf selbst `*-sys` erreichen, liegt aber in der
        // Hülle von `j`: auch die Build-Kanten transitiv erreichter interner
        // Knoten gehören zur Hülle.
        let mut i = node("i", &["f"], &[]);
        i.build_deps = vec!["openssl-sys".to_owned()];
        let lock = format!("{LOCK}\n[[package]]\nname = \"openssl-sys\"\nversion = \"0.9.0\"\n");

        let report = run_with(base_crates_with(i), POLICY, &lock)?;

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("'openssl-sys'") && v.contains("j → i → openssl-sys")),
            "die Meldung muss den Pfad über die Build-Kante von i nennen: {:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_build_dependency_without_lock_entry_fails_closed() -> TestResult {
        // Das TCB-Paket `t` nennt `bindgen` als `[build-dependencies]`, das
        // weder Workspace-Knoten noch in `Cargo.lock` ist: die Hülle wäre
        // unvollständig, und der C-Bauhelfer selbst ist verboten.
        let mut t = node("t", &["f"], &[]);
        t.build_deps = vec!["bindgen".to_owned()];

        let report = run_with(base_crates_with(t), POLICY, LOCK)?;

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("t: 'bindgen' hat keinen Eintrag in Cargo.lock")),
            "{:?}",
            report.violations
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("t erreicht") && v.contains("'bindgen'") && v.contains("TCB")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_dev_dependency_is_not_in_sys_hull() -> TestResult {
        // `j` nennt `openssl-sys` nur als `[dev-dependencies]`: das landet in
        // keinem ausgelieferten Artefakt und gehört nicht zur Hülle.
        let mut j = node("j", &["f", "i"], &["tokio"]);
        j.dev_deps = vec!["openssl-sys".to_owned()];
        let lock = format!("{LOCK}\n[[package]]\nname = \"openssl-sys\"\nversion = \"0.9.0\"\n");

        let report = run_with(base_crates_with(j), POLICY, &lock)?;

        assert!(report.is_green(), "{:?}", report.violations);
        Ok(())
    }

    #[test]
    fn test_evaluate_external_without_lock_entry_is_a_violation() -> TestResult {
        let report = run_with(base_crates(), POLICY, "version = 4\n")?;

        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("'tokio' hat keinen Eintrag in Cargo.lock")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn test_evaluate_empty_graph_is_red_with_zero_checked() -> TestResult {
        let report = run_with(Vec::new(), POLICY, LOCK)?;

        assert_eq!(report.checked, 0);
        assert!(!report.is_green(), "leerer Graph prüft nichts (G-102)");
        Ok(())
    }

    #[test]
    fn test_parse_rejects_unknown_layer_in_package() {
        let policy = POLICY.replace(
            "[packages.\"a\"]\nlayer = \"A\"",
            "[packages.\"a\"]\nlayer = \"X\"",
        );

        let result = Policy::parse(&policy);

        assert!(
            result
                .as_ref()
                .is_err_and(|e| e.contains("unbekannte Schicht 'X'")),
            "{result:?}"
        );
    }

    #[test]
    fn test_parse_rejects_misspelled_key() {
        let policy = POLICY.replace("layer = \"I\"\ntcb = true", "layer = \"I\"\ntbc = true");

        let result = Policy::parse(&policy);

        assert!(
            result.as_ref().is_err_and(|e| e.contains("'tbc'")),
            "ein Tippfehler darf nicht still als 'kein TCB' gelesen werden: {result:?}"
        );
    }

    #[test]
    fn test_parse_rejects_tcb_package_without_allowlist() {
        let policy = POLICY.replace("[tcb.\"t\"]\nallowed_internal = [\"f\"]\n", "");

        let result = Policy::parse(&policy);

        assert!(
            result
                .as_ref()
                .is_err_and(|e| e.contains("[tcb.\"t\"] fehlt")),
            "{result:?}"
        );
    }

    #[test]
    fn test_parse_rejects_layer_without_rule() {
        let policy = POLICY.replace("A.may_depend_on = [\"*\"]\n", "");

        let result = Policy::parse(&policy);

        assert!(
            result
                .as_ref()
                .is_err_and(|e| e.contains("'A' hat keine Regel")),
            "{result:?}"
        );
    }

    #[test]
    fn test_lock_index_strips_version_suffix() -> TestResult {
        let lock = LockIndex::parse(
            "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\ndependencies = [\"toml 1.1.3+spec-1.1.0\", \"serde\"]\n",
        )
        .map_err(TestError::Unexpected)?;

        let deps = lock.deps.get("x").ok_or(TestError::Missing("Eintrag x"))?;

        assert!(deps.contains("toml") && deps.contains("serde"), "{deps:?}");
        Ok(())
    }

    /// Das echte Repository muss grün sein: jede Crate klassifiziert, jede
    /// Inversion als Ausnahme begründet, keine veraltete Ausnahme, und keine
    /// `[[forbidden_crates]]`-Hülle erreicht einen verbotenen oder
    /// unauflösbaren Namen — geprüft wie in [`run`], mit den aufgelösten
    /// Manifest-Kanten (Target-Tabellen, Umbenennungen).
    #[test]
    fn test_real_repository_is_green() -> TestResult {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("Repo-Wurzel über xtask/"))?;
        let graph = super::super::load_all_workspaces(repo_root).map_err(TestError::Unexpected)?;
        let policy = load_policy(repo_root).map_err(TestError::Unexpected)?;
        let lock = LockIndex::load(repo_root).map_err(TestError::Unexpected)?;
        let manifests = ManifestEdges::load(repo_root).map_err(TestError::Unexpected)?;

        let report = evaluate_with_manifests(&graph, &policy, &lock, &manifests);

        assert!(report.is_green(), "{:#?}", report.violations);
        // Die Manifest-Kanten decken jedes Paket des Graphen ab; sonst fiele
        // die Verbots-Hülle still auf die Graph-Schlüssel zurück.
        for node in &graph.crates {
            assert!(
                manifests.deps_of(&node.name).is_some(),
                "{} fehlt in ManifestEdges",
                node.name
            );
        }
        // harw-job-runtime deklariert harw-job-linux nur unter
        // `[target.'cfg(target_os = "linux")'.dependencies]`; der Graph sieht
        // die Kante nicht, die Verbots-Hülle schon.
        let job_runtime = manifests
            .deps_of("harw-job-runtime")
            .ok_or(TestError::Missing("harw-job-runtime in ManifestEdges"))?;
        assert!(job_runtime.contains("harw-job-linux"), "{job_runtime:?}");
        Ok(())
    }

    /// Baut [`ManifestEdges`] aus TOML-Texten: `root` ist die Wurzel,
    /// `packages` je `(Paketname, Manifest)`.
    fn manifest_edges(root: &str, packages: &[(&str, &str)]) -> TestResult<ManifestEdges> {
        let root: toml::Table = toml::from_str(root).map_err(ctx("Wurzel-Manifest parsen"))?;
        let mut parsed = Vec::with_capacity(packages.len());
        for (name, text) in packages {
            let manifest: toml::Table = toml::from_str(text).map_err(ctx("Manifest parsen"))?;
            parsed.push(((*name).to_owned(), manifest));
        }
        Ok(ManifestEdges::from_manifests(&root, &parsed))
    }

    /// Die echte Richtlinie aus [`POLICY_PATH`].
    fn real_policy() -> TestResult<Policy> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("Repo-Wurzel über xtask/"))?;
        load_policy(repo_root).map_err(TestError::Unexpected)
    }

    /// [`ManifestEdges`] liest jede ausgelieferte Tabelle (auch unter
    /// `[target.'…']` und in Unterstrich-Schreibweise) unter dem Paketnamen
    /// des Eintrags und lässt Dev-Tabellen weg.
    #[test]
    fn test_manifest_edges_resolve_packages_target_tables_and_inheritance() -> TestResult {
        let edges = manifest_edges(
            r#"
[workspace.dependencies]
signing = { package = "harw-dod-warden-proto", path = "w" }
plain = { path = "p" }
both = { package = "root-name", path = "b" }
"#,
            &[(
                "a",
                r#"
[package]
name = "a"

[dependencies]
renamed = { package = "real-renamed", path = "../r" }
signing = { workspace = true }
plain = { workspace = true }
both = { workspace = true, package = "local-name" }
missing-root = { workspace = true }
direct = "1"

[build-dependencies]
builder = "1"

[build_dependencies]
old-builder = "1"

[dev-dependencies]
dev-only = "1"

[target.'cfg(target_os = "linux")'.dependencies]
linux-only = { path = "../l" }

[target.'cfg(unix)'.build-dependencies]
unix-builder = "1"

[target.'cfg(unix)'.dev-dependencies]
unix-dev = "1"
"#,
            )],
        )?;

        let deps = edges.deps_of("a").ok_or(TestError::Missing("a"))?;
        let expected: BTreeSet<String> = [
            "real-renamed",
            "harw-dod-warden-proto",
            "plain",
            "root-name",
            "local-name",
            "missing-root",
            "direct",
            "builder",
            "old-builder",
            "linux-only",
            "unix-builder",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(deps, &expected);
        assert!(edges.deps_of("b").is_none());
        Ok(())
    }

    /// Eine Wurzel mit `[package]` ist selbst ein Paket und bekommt Kanten.
    #[test]
    fn test_manifest_edges_count_a_root_package() -> TestResult {
        let edges = manifest_edges(
            "[package]\nname = \"root-pkg\"\n\n[dependencies]\nx = \"1\"\n",
            &[],
        )?;

        let deps = edges
            .deps_of("root-pkg")
            .ok_or(TestError::Missing("root-pkg"))?;
        assert_eq!(deps, &BTreeSet::from(["x".to_owned()]));
        Ok(())
    }

    /// B16: eine **umbenannte** Kante von harw-runtime auf den Signierteil
    /// (`signing = { package = "harw-dod-warden-proto" }`) ist rot. Der Graph
    /// kennt nur den Schlüssel `signing`; die Manifest-Kanten lösen ihn auf.
    #[test]
    fn test_renamed_edge_to_the_signing_crate_is_red() -> TestResult {
        let policy = real_policy()?;
        let edges = manifest_edges(
            "[workspace]\n",
            &[(
                "harw-runtime",
                "[package]\nname = \"harw-runtime\"\n\n[dependencies]\nsigning = { package = \"harw-dod-warden-proto\", path = \"../w\" }\n",
            )],
        )?;
        let crates = graph(vec![
            node("harw-runtime", &[], &["signing"]),
            node("harw-dod-warden-proto", &[], &[]),
        ]);

        let report = evaluate_with_manifests(&crates, &policy, &LockIndex::default(), &edges);

        assert_hits(
            &forbidden_hits(&report),
            &[
                "harw-runtime erreicht die verbotene Crate 'harw-dod-warden-proto' über harw-runtime → harw-dod-warden-proto —",
            ],
        );
        Ok(())
    }

    /// B16: auch über `workspace = true` geerbt und umbenannt ist die Kante
    /// rot (harw-ops → `harw-dod-escalate` über den Wurzeleintrag `ladder`).
    #[test]
    fn test_inherited_renamed_edge_to_escalate_is_red() -> TestResult {
        let policy = real_policy()?;
        let edges = manifest_edges(
            "[workspace.dependencies]\nladder = { package = \"harw-dod-escalate\", path = \"e\" }\n",
            &[(
                "harw-ops",
                "[package]\nname = \"harw-ops\"\n\n[dependencies]\nladder = { workspace = true }\n",
            )],
        )?;
        let crates = graph(vec![
            node("harw-ops", &[], &["ladder"]),
            node("harw-dod-escalate", &[], &[]),
        ]);

        let report = evaluate_with_manifests(&crates, &policy, &LockIndex::default(), &edges);

        assert_hits(
            &forbidden_hits(&report),
            &[
                "harw-ops erreicht die verbotene Crate 'harw-dod-escalate' über harw-ops → harw-dod-escalate —",
            ],
        );
        Ok(())
    }

    /// B16: eine Kante nur unter `[target.'cfg(target_os = "linux")'.dependencies]`
    /// ist rot — direkt von harw-runtime und transitiv über ein Paket, das
    /// seine Linux-Kanten nur dort deklariert (wie harw-job-runtime). Der
    /// Graph sieht keine der beiden Kanten.
    #[test]
    fn test_target_only_edges_to_signing_crates_are_red() -> TestResult {
        let policy = real_policy()?;
        let edges = manifest_edges(
            "[workspace]\n",
            &[
                (
                    "harw-runtime",
                    r#"
[package]
name = "harw-runtime"

[dependencies]
harw-job-runtime = { path = "../harw-job-runtime" }

[target.'cfg(target_os = "linux")'.dependencies]
harw-dod-warden-proto = { path = "../w" }
"#,
                ),
                (
                    "harw-job-runtime",
                    r#"
[package]
name = "harw-job-runtime"

[target.'cfg(target_os = "linux")'.dependencies]
harw-dod-escalate = { path = "../e" }
"#,
                ),
            ],
        )?;
        let crates = graph(vec![
            node("harw-runtime", &["harw-job-runtime"], &[]),
            node("harw-job-runtime", &[], &[]),
            node("harw-dod-warden-proto", &[], &[]),
            node("harw-dod-escalate", &[], &[]),
        ]);

        let blind = evaluate(&crates, &policy, &LockIndex::default());
        assert!(
            forbidden_hits(&blind).is_empty(),
            "der Graph allein sieht keine Target-Kante: {:#?}",
            blind.violations
        );

        let report = evaluate_with_manifests(&crates, &policy, &LockIndex::default(), &edges);

        assert_hits(
            &forbidden_hits(&report),
            &[
                "harw-runtime erreicht die verbotene Crate 'harw-dod-warden-proto' über harw-runtime → harw-dod-warden-proto —",
                "harw-runtime erreicht die verbotene Crate 'harw-dod-escalate' über harw-runtime → harw-job-runtime → harw-dod-escalate —",
            ],
        );
        Ok(())
    }

    /// Dev-Abhängigkeiten (auch unter `[target.'…']`) landen in keinem
    /// ausgelieferten Artefakt und bleiben aus der Verbots-Hülle.
    #[test]
    fn test_dev_only_edges_to_signing_crates_stay_green() -> TestResult {
        let policy = real_policy()?;
        let edges = manifest_edges(
            "[workspace]\n",
            &[(
                "harw-runtime",
                r#"
[package]
name = "harw-runtime"

[dev-dependencies]
harw-dod-warden-proto = { path = "../w" }

[target.'cfg(unix)'.dev-dependencies]
harw-dod-escalate = { path = "../e" }
"#,
            )],
        )?;
        let crates = graph(vec![
            node("harw-runtime", &[], &[]),
            node("harw-dod-warden-proto", &[], &[]),
            node("harw-dod-escalate", &[], &[]),
        ]);

        let report = evaluate_with_manifests(&crates, &policy, &LockIndex::default(), &edges);

        let hits = forbidden_hits(&report);
        assert!(hits.is_empty(), "{hits:#?}");
        Ok(())
    }

    /// Fail closed in jeder Schicht: ein Name in einer Verbots-Hülle, der
    /// weder lokales Paket noch in `Cargo.lock` ist (hier der umbenannte
    /// Schlüssel ohne Manifest-Kanten), ist rot — auch für ein Paket der
    /// Schicht A, für das die `*-sys`-Regel nicht gilt. Gelten mehrere Zeilen
    /// (harw-dod-submit-proto steht in drei), erscheint die Meldung trotzdem
    /// nur einmal.
    #[test]
    fn test_unresolved_name_in_a_forbidden_hull_fails_closed() -> TestResult {
        let policy = real_policy()?;
        let crates = graph(vec![
            node("harw-runtime", &[], &["signing"]),
            node("harw-dod-submit-proto", &[], &["signing"]),
        ]);
        let submission_lines = policy
            .forbidden
            .iter()
            .filter(|rule| {
                policy
                    .packages
                    .get("harw-dod-submit-proto")
                    .is_some_and(|class| rule.applies_to("harw-dod-submit-proto", class))
            })
            .count();
        assert!(submission_lines >= 2, "{submission_lines}");

        let report = evaluate(&crates, &policy, &LockIndex::default());

        let runtime = "harw-runtime: 'signing' ist weder lokales Paket noch in Cargo.lock";
        let submission =
            "harw-dod-submit-proto: 'signing' ist weder lokales Paket noch in Cargo.lock";
        assert!(
            report.violations.iter().any(|v| v.starts_with(runtime)),
            "{:#?}",
            report.violations
        );
        assert_eq!(
            report
                .violations
                .iter()
                .filter(|v| v.starts_with(submission))
                .count(),
            1,
            "{:#?}",
            report.violations
        );
        Ok(())
    }

    /// Fail closed auch auf dem Pfad, den [`run`] nimmt: ein Name, der nur in
    /// den [`ManifestEdges`] steht (hier umbenannt unter `[target.'…']`), der
    /// weder lokales Paket noch in `Cargo.lock` ist, ist über
    /// [`evaluate_with_manifests`] genau einmal rot. Die graphbasierte
    /// Kurzform sieht die Target-Kante nicht und meldet ihn deshalb nicht.
    #[test]
    fn test_unresolved_manifest_name_fails_closed_through_evaluate_with_manifests() -> TestResult {
        let policy = real_policy()?;
        let edges = manifest_edges(
            "[workspace]\n",
            &[(
                "harw-runtime",
                "[package]\nname = \"harw-runtime\"\n\n[target.'cfg(unix)'.dependencies]\nx = { package = \"ghost-crate\", path = \"../g\" }\n",
            )],
        )?;
        let crates = graph(vec![node("harw-runtime", &[], &[])]);
        let ghost = "harw-runtime: 'ghost-crate' ist weder lokales Paket noch in Cargo.lock";

        let report = evaluate_with_manifests(&crates, &policy, &LockIndex::default(), &edges);

        assert_eq!(
            report
                .violations
                .iter()
                .filter(|v| v.starts_with(ghost))
                .count(),
            1,
            "{:#?}",
            report.violations
        );
        let graph_only = evaluate(&crates, &policy, &LockIndex::default());
        assert!(
            !graph_only
                .violations
                .iter()
                .any(|v| v.contains("ghost-crate")),
            "{:#?}",
            graph_only.violations
        );
        Ok(())
    }

    /// Die `[[forbidden_crates]]`-Treffer eines Berichts; andere Verstöße
    /// (etwa veraltete TCB-Zeilen in einem konstruierten Graphen) bleiben
    /// draußen.
    fn forbidden_hits(report: &GateReport) -> Vec<&str> {
        report
            .violations
            .iter()
            .map(String::as_str)
            .filter(|violation| violation.contains("[[forbidden_crates]]"))
            .collect()
    }

    /// Prüft, dass zu jedem erwarteten Präfix ein Treffer existiert.
    fn assert_hits(hits: &[&str], expected: &[&str]) {
        for &prefix in expected {
            assert!(
                hits.iter().any(|hit| hit.starts_with(prefix)),
                "erwartet: {prefix}\nTreffer: {hits:#?}"
            );
        }
    }

    /// PL-100 WP 3.0 / Leitprinzip I8: die **echten** Verbots-Zeilen aus
    /// [`POLICY_PATH`] gegen konstruierte Graphen. Der Pfad vor WP 3.0
    /// (harw-runtime → harw-plan-bridge → harw-dod-escalate →
    /// harw-dod-warden-proto) ist an jeder Station rot, ebenso eine Andockung
    /// über die Leiter (harw-plan-bridge → harw-dod-ladder). Grün ist die
    /// Andockung über die signierfreien Crates `harw-dod-rules`,
    /// `harw-dod-submit-proto` und `harw-dod-warden-types`.
    #[test]
    fn test_real_policy_keeps_signing_code_out_of_model_facing_packages() -> TestResult {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("Repo-Wurzel über xtask/"))?;
        let policy = load_policy(repo_root).map_err(TestError::Unexpected)?;
        let lock = LockIndex::default();

        let before = evaluate(
            &graph(vec![
                node("harw-runtime", &["harw-plan-bridge"], &[]),
                node("harw-plan-bridge", &["harw-dod-escalate"], &[]),
                node("harw-dod-escalate", &["harw-dod-warden-proto"], &[]),
                node("harw-dod-warden-proto", &[], &[]),
            ]),
            &policy,
            &lock,
        );
        assert_hits(
            &forbidden_hits(&before),
            &[
                "harw-runtime erreicht die verbotene Crate 'harw-dod-escalate' über harw-runtime → harw-plan-bridge → harw-dod-escalate —",
                "harw-runtime erreicht die verbotene Crate 'harw-dod-warden-proto' über harw-runtime → harw-plan-bridge → harw-dod-escalate → harw-dod-warden-proto —",
                "harw-plan-bridge erreicht die verbotene Crate 'harw-dod-escalate' über harw-plan-bridge → harw-dod-escalate —",
                "harw-plan-bridge erreicht die verbotene Crate 'harw-dod-warden-proto' über harw-plan-bridge → harw-dod-escalate → harw-dod-warden-proto —",
                "harw-dod-escalate erreicht die verbotene Crate 'harw-dod-warden-proto' über harw-dod-escalate → harw-dod-warden-proto —",
            ],
        );

        // Auch die signierfreie Leiter liegt außerhalb modellnaher Prozesse.
        let ladder_docking = evaluate(
            &graph(vec![
                node("harw-runtime", &["harw-plan-bridge"], &[]),
                node("harw-plan-bridge", &["harw-dod-ladder"], &[]),
                node("harw-dod-ladder", &["harw-dod-warden-types"], &[]),
                node("harw-dod-warden-types", &[], &[]),
            ]),
            &policy,
            &lock,
        );
        let ladder_hits = forbidden_hits(&ladder_docking);
        assert_hits(
            &ladder_hits,
            &[
                "harw-runtime erreicht die verbotene Crate 'harw-dod-ladder' über harw-runtime → harw-plan-bridge → harw-dod-ladder —",
                "harw-plan-bridge erreicht die verbotene Crate 'harw-dod-ladder' über harw-plan-bridge → harw-dod-ladder —",
            ],
        );
        assert!(
            !ladder_hits
                .iter()
                .any(|hit| hit.contains("'harw-dod-warden-types'")),
            "das signierfreie Vokabular bleibt erlaubt: {ladder_hits:#?}"
        );

        let docking = evaluate(
            &graph(vec![
                node("harw-runtime", &["harw-plan-bridge"], &[]),
                node(
                    "harw-plan-bridge",
                    &[
                        "harw-dod-rules",
                        "harw-dod-submit-proto",
                        "harw-dod-warden-types",
                    ],
                    &[],
                ),
                node("harw-dod-rules", &[], &[]),
                node("harw-dod-submit-proto", &[], &[]),
                node("harw-dod-warden-types", &[], &[]),
            ]),
            &policy,
            &lock,
        );
        let docking_hits = forbidden_hits(&docking);
        assert!(docking_hits.is_empty(), "{docking_hits:#?}");

        // Der angekündigte Escalator ist verboten, sobald es ihn gibt —
        // auch über eine sonst nur der Eskalationsseite erlaubte Crate.
        let escalator = evaluate(
            &graph(vec![
                node("harw-core", &["harw-dod-ladder"], &[]),
                node("harw-ops", &["harw-dod-ladder"], &[]),
                node("harw-dod-ladder", &["harw-escalator"], &[]),
                node("harw-escalator", &[], &[]),
            ]),
            &policy,
            &lock,
        );
        assert_hits(
            &forbidden_hits(&escalator),
            &[
                "harw-core erreicht die verbotene Crate 'harw-dod-ladder' über harw-core → harw-dod-ladder —",
                "harw-core erreicht die verbotene Crate 'harw-escalator' über harw-core → harw-dod-ladder → harw-escalator —",
                "harw-ops erreicht die verbotene Crate 'harw-dod-ladder' über harw-ops → harw-dod-ladder —",
                "harw-ops erreicht die verbotene Crate 'harw-escalator' über harw-ops → harw-dod-ladder → harw-escalator —",
                "harw-dod-ladder erreicht die verbotene Crate 'harw-escalator' über harw-dod-ladder → harw-escalator —",
            ],
        );
        Ok(())
    }

    #[test]
    fn forbidden_crate_in_a_layer_hull_is_a_violation() -> TestResult {
        let policy = Policy::parse(&format!(
            "{POLICY}\n[[forbidden_crates]]\nnames = [\"mio\"]\nlayers = [\"J\"]\nreason = \"test\"\n"
        ))
        .map_err(TestError::Unexpected)?;
        let lock = LockIndex::parse(LOCK).map_err(TestError::Unexpected)?;
        let report = evaluate(&graph(base_crates()), &policy, &lock);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("j erreicht die verbotene Crate 'mio' über j → tokio → mio")),
            "{:?}",
            report.violations
        );
        Ok(())
    }

    #[test]
    fn forbidden_crate_rule_can_target_single_packages() -> TestResult {
        let policy = Policy::parse(&format!(
            "{POLICY}\n[[forbidden_crates]]\nnames = [\"tokio\"]\npackages = [\"f\", \"j\"]\nreason = \"no runtime\"\n"
        ))
        .map_err(TestError::Unexpected)?;
        let lock = LockIndex::parse(LOCK).map_err(TestError::Unexpected)?;
        let report = evaluate(&graph(base_crates()), &policy, &lock);
        let hits: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.contains("verbotene Crate 'tokio'"))
            .collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].starts_with("j "));
        Ok(())
    }

    #[test]
    fn forbidden_crates_reject_unknown_layers_and_packages() {
        for extra in [
            "[[forbidden_crates]]\nnames = [\"x\"]\nlayers = [\"Q\"]\nreason = \"r\"\n",
            "[[forbidden_crates]]\nnames = [\"x\"]\npackages = [\"nope\"]\nreason = \"r\"\n",
            "[[forbidden_crates]]\nnames = [\"x\"]\nreason = \"r\"\n",
            "[[forbidden_crates]]\nnames = [\"x\"]\nlayers = [\"J\"]\nreason = \"r\"\ntypo = 1\n",
        ] {
            assert!(
                Policy::parse(&format!("{POLICY}\n{extra}")).is_err(),
                "{extra}"
            );
        }
    }
}
