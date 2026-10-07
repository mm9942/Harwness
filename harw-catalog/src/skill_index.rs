//! `SkillIndex` — der lesende Skill-Katalog für die Modellwerkzeuge
//! `skills.search` und `skills.load`.
//!
//! # Verantwortung
//! Ein Agent soll jeden verfügbaren Skill **finden** und vor der Arbeit
//! **laden** können, ohne dass alle Skills vorab im Prompt stehen. Der Index
//! sammelt dafür einmal je Montage:
//! - die Skills der vertrauten Config-Layer (`skills/*/skill.toml`, dieselbe
//!   Präzedenz wie `harw_config::discover_config` und
//!   [`crate::resolve_skill_directory`]: der **letzte** Layer gewinnt,
//!   innerhalb eines Layers das nach Verzeichnisnamen letzte Manifest), und
//! - als Rückfall die in `harw-home` eingebetteten Skills
//!   ([`harw_home::bundled_files`]) für jeden Namen, den kein Layer führt.
//!   Damit sind mitgelieferte Skills nie „einkompiliert, aber nicht da“ —
//!   auch ohne Scaffold (`~/.harw/skills` fehlt, anderes `HARW_HOME`).
//!
//! Ein deaktivierter Skill (`enabled = false`) fehlt im Index; er verdeckt
//! zugleich den gleichnamigen eingebetteten Skill (die Nutzerin hat ihn
//! bewusst abgeschaltet).
//!
//! # Sicherheit
//! Rein lesend. Anweisungstexte von der Platte laufen über
//! [`crate::load_skill_runtime_snapshot`] (Symlink- und Traversal-Schutz,
//! 512-KiB-Deckel). Deklarierte Werkzeuge eines Skills sind nur Text — sie
//! verleihen keine Rechte.
//!
//! # Nebenläufigkeit
//! Nach dem Bau unveränderlich; `Send + Sync`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use harw_config::SkillToml;

use crate::skill_triggers::{
    MAX_TRIGGERS_FILE_BYTES, SkillTriggerSpec, SkillTriggers, SkillWarning, TriggerIndex,
    parse_triggers_file,
};
use crate::{SkillRuntimeSnapshot, load_skill_runtime_snapshot, sha256_hex};

/// Dateiname der optionalen Trigger-Datei neben `skill.toml`.
pub const TRIGGERS_FILE: &str = "triggers.toml";

/// Anzeigename der Quelle eines eingebetteten Skills.
pub const BUNDLED_SKILL_SOURCE: &str = "eingebaut";

/// Obergrenze des Textes, den `skills.load` in einem Aufruf liefert.
pub const MAX_SKILL_LOAD_BYTES: usize = 64 * 1024;

/// Höchstlänge der Kurzbeschreibung in Suchergebnissen (Zeichen).
pub const SHORT_DESCRIPTION_CHARS: usize = 160;

/// Obergrenze des Anweisungstexts eines eingebetteten Skills (wie auf der
/// Platte).
const MAX_INSTRUCTIONS_BYTES: usize = 512 * 1024;

/// Woher ein Skill im Index stammt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// Aus einem vertrauten Config-Layer; der Pfad ist das Skill-Verzeichnis.
    Layer(PathBuf),
    /// Aus dem in `harw-home` eingebetteten Bündel.
    Bundled,
}

impl SkillSource {
    /// Die Anzeige der Quelle: das Skill-Verzeichnis bzw.
    /// [`BUNDLED_SKILL_SOURCE`].
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Layer(dir) => dir.display().to_string(),
            Self::Bundled => BUNDLED_SKILL_SOURCE.to_owned(),
        }
    }
}

/// Eine Überschrift des Anweisungstexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillHeading {
    /// Ebene (Anzahl `#`).
    pub level: usize,
    /// Text ohne `#` und Rand-Leerzeichen.
    pub text: String,
}

/// Ein Eintrag des Skill-Index.
#[derive(Debug, Clone)]
pub struct SkillIndexEntry {
    /// Skill-Name aus dem Manifest.
    pub name: String,
    /// Beschreibung aus dem Manifest.
    pub description: String,
    /// Deklarierte Werkzeuge (keine Freigabe).
    pub tools: Vec<String>,
    /// Deklarierte MCPs (keine Freigabe).
    pub mcps: Vec<String>,
    /// Herkunft.
    pub source: SkillSource,
    /// Größe des Anweisungstexts in Bytes.
    pub size_bytes: usize,
    /// Alle Überschriften des Anweisungstexts (außerhalb von Codeblöcken).
    pub headings: Vec<SkillHeading>,
    /// Der `when`-Satz aus `triggers.toml` (<= 160 Zeichen), falls gültig.
    pub when: Option<String>,
    /// Die geprüften Trigger aus `triggers.toml` (leer, wenn es keine gibt).
    pub triggers: SkillTriggers,
    snapshot: SkillRuntimeSnapshot,
}

impl SkillIndexEntry {
    fn new(snapshot: SkillRuntimeSnapshot, source: SkillSource, spec: SkillTriggerSpec) -> Self {
        let headings = markdown_headings(&snapshot.instructions)
            .into_iter()
            .map(|(_, level, text)| SkillHeading { level, text })
            .collect();
        Self {
            name: snapshot.name.clone(),
            description: snapshot.description.clone(),
            tools: snapshot.tools.clone(),
            mcps: snapshot.mcps.clone(),
            source,
            size_bytes: snapshot.instructions.len(),
            headings,
            when: spec.when,
            triggers: spec.triggers,
            snapshot,
        }
    }

    /// Der Satz der L0-Katalogzeile: `when`, sonst der erste Satz der
    /// Beschreibung ([`Self::short_description`]).
    #[must_use]
    pub fn catalog_sentence(&self) -> String {
        self.when
            .clone()
            .unwrap_or_else(|| self.short_description())
    }

    /// Der eingefrorene Laufzeit-Snapshot (Anweisungen samt SHA-256).
    #[must_use]
    pub fn snapshot(&self) -> &SkillRuntimeSnapshot {
        &self.snapshot
    }

    /// Das vollständige Instruktionsfragment, gleich gerahmt wie
    /// [`SkillRuntimeSnapshot::instruction_fragment`].
    #[must_use]
    pub fn fragment(&self) -> String {
        self.snapshot.instruction_fragment()
    }

    /// Der erste Satz der Beschreibung, höchstens
    /// [`SHORT_DESCRIPTION_CHARS`] Zeichen (mit `…` gekürzt).
    #[must_use]
    pub fn short_description(&self) -> String {
        first_sentence(&self.description, SHORT_DESCRIPTION_CHARS)
    }

    /// Die `##`-Abschnitte des Anweisungstexts, in Dokumentreihenfolge.
    #[must_use]
    pub fn sections(&self) -> Vec<String> {
        self.headings
            .iter()
            .filter(|heading| heading.level == 2)
            .map(|heading| heading.text.clone())
            .collect()
    }

    /// Ein einzelner Abschnitt als Fragment: Kopfzeile wie
    /// [`Self::fragment`] (Name, SHA-256 des ganzen Skills), danach der
    /// Abschnitt ab seiner Überschrift bis vor die nächste Überschrift
    /// gleicher oder höherer Ebene.
    ///
    /// # Beschreibung
    /// Gesucht wird unter den Überschriften der Ebenen 2 und 3: zuerst exakt
    /// (ohne Groß-/Kleinschreibung), dann als Teilzeichenkette. `None`, wenn
    /// keine Überschrift passt.
    #[must_use]
    pub fn section_fragment(&self, section: &str) -> Option<String> {
        let wanted = section.trim().trim_start_matches('#').trim().to_lowercase();
        if wanted.is_empty() {
            return None;
        }
        let text = &self.snapshot.instructions;
        let headings = markdown_headings(text);
        let candidates: Vec<&(usize, usize, String)> = headings
            .iter()
            .filter(|(_, level, _)| (2..=3).contains(level))
            .collect();
        let chosen = candidates
            .iter()
            .find(|(_, _, heading)| heading.to_lowercase() == wanted)
            .or_else(|| {
                candidates
                    .iter()
                    .find(|(_, _, heading)| heading.to_lowercase().contains(&wanted))
            })?;
        let (start, level, heading) = (chosen.0, chosen.1, &chosen.2);
        let end = headings
            .iter()
            .find(|(offset, other_level, _)| *offset > start && *other_level <= level)
            .map_or(text.len(), |(offset, _, _)| *offset);
        let body = text.get(start..end)?.trim_end();
        Some(format!(
            "# Skill: {} (sha256 {}) — Abschnitt „{heading}“\n\n{body}\n",
            self.name, self.snapshot.sha256
        ))
    }
}

/// Ein beim Bau übersprungener Skill (nicht lesbar oder ungültig).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedSkill {
    /// Wo der Skill lag (Verzeichnis bzw. Bündelpfad).
    pub location: String,
    /// Warum er fehlt.
    pub reason: String,
}

/// Ein Suchtreffer von [`SkillIndex::search`].
#[derive(Debug, Clone, Copy)]
pub struct SkillSearchHit<'a> {
    /// Der gefundene Eintrag.
    pub entry: &'a SkillIndexEntry,
    /// Relevanz (größer ist besser).
    pub score: u32,
}

/// Der lesende Skill-Katalog über vertraute Layer und das eingebettete
/// Bündel (siehe Moduldoku).
#[derive(Debug, Clone, Default)]
pub struct SkillIndex {
    entries: BTreeMap<String, SkillIndexEntry>,
    disabled: BTreeSet<String>,
    skipped: Vec<SkippedSkill>,
    warnings: Vec<SkillWarning>,
    triggers: TriggerIndex,
}

/// Die eingebetteten Skill-Dateien aus `harw-home` als
/// `(relativer Pfad, Inhalt)`.
#[must_use]
pub fn bundled_skill_files() -> Vec<(&'static str, &'static str)> {
    harw_home::bundled_files()
        .iter()
        .filter(|file| file.relative_path.starts_with("skills/"))
        .map(|file| (file.relative_path, file.contents))
        .collect()
}

impl SkillIndex {
    /// Baut den Index über `layers` (aufsteigende Präzedenz, wie
    /// `ConfigTrustReport::layers`) mit dem eingebetteten Bündel als
    /// Rückfall.
    ///
    /// # Beschreibung
    /// Scheitert nie: ein nicht lesbarer oder ungültiger Skill wird
    /// übersprungen und in [`Self::skipped`] vermerkt.
    #[must_use]
    pub fn build(layers: &[PathBuf]) -> Self {
        Self::build_with_bundle(layers, &bundled_skill_files())
    }

    /// Wie [`Self::build`], aber mit einem ausdrücklich übergebenen Bündel
    /// (`(relativer Pfad, Inhalt)`, Pfade wie `skills/<dir>/skill.toml`).
    #[must_use]
    pub fn build_with_bundle(layers: &[PathBuf], bundle: &[(&str, &str)]) -> Self {
        let mut index = Self::default();
        let mut winners: BTreeMap<String, (PathBuf, SkillToml)> = BTreeMap::new();
        for layer in layers {
            index.scan_layer(layer, &mut winners);
        }
        for (name, (dir, manifest)) in winners {
            if !manifest.enabled {
                index.disabled.insert(name);
                continue;
            }
            match load_skill_runtime_snapshot(&dir, &manifest) {
                Ok(snapshot) => {
                    let spec = index.trigger_spec(&name, read_layer_triggers(&dir));
                    index.entries.insert(
                        name,
                        SkillIndexEntry::new(snapshot, SkillSource::Layer(dir), spec),
                    );
                }
                Err(error) => index.skipped.push(SkippedSkill {
                    location: dir.display().to_string(),
                    reason: error.to_string(),
                }),
            }
        }
        index.add_bundle(bundle);
        index.triggers = TriggerIndex::from_entries(
            index
                .entries
                .values()
                .map(|entry| (entry.name.as_str(), &entry.triggers)),
        );
        index
    }

    /// Prüft den Inhalt einer `triggers.toml`; Fehler und verworfene Teile
    /// landen als Warnung, der Skill bleibt geladen.
    fn trigger_spec(
        &mut self,
        skill: &str,
        source: Option<Result<String, String>>,
    ) -> SkillTriggerSpec {
        let (spec, reasons) = match source {
            None => return SkillTriggerSpec::default(),
            Some(Ok(text)) => parse_triggers_file(&text),
            Some(Err(reason)) => (SkillTriggerSpec::default(), vec![reason]),
        };
        self.warnings
            .extend(reasons.into_iter().map(|reason| SkillWarning {
                skill: skill.to_owned(),
                reason,
            }));
        spec
    }

    /// Liest `skills/*/skill.toml` eines Layers in aufsteigender
    /// Verzeichnisnamen-Reihenfolge; ein späteres Manifest überschreibt ein
    /// früheres gleichen Namens.
    fn scan_layer(&mut self, layer: &Path, winners: &mut BTreeMap<String, (PathBuf, SkillToml)>) {
        let skills_dir = layer.join("skills");
        let Ok(read_dir) = std::fs::read_dir(&skills_dir) else {
            return;
        };
        let mut dirs: Vec<(std::ffi::OsString, PathBuf)> = read_dir
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| (entry.file_name(), entry.path()))
            .collect();
        dirs.sort();
        for (_, dir) in dirs {
            let manifest_path = dir.join("skill.toml");
            if !manifest_path.is_file() {
                continue;
            }
            let parsed = std::fs::read_to_string(&manifest_path)
                .map_err(|error| error.to_string())
                .and_then(|source| {
                    toml::from_str::<SkillToml>(&source).map_err(|error| error.to_string())
                });
            match parsed {
                Ok(manifest) => {
                    winners.insert(manifest.name.clone(), (dir, manifest));
                }
                Err(reason) => self.skipped.push(SkippedSkill {
                    location: manifest_path.display().to_string(),
                    reason,
                }),
            }
        }
    }

    /// Ergänzt jeden eingebetteten Skill, den kein Layer führt (weder
    /// aktiviert noch deaktiviert).
    fn add_bundle(&mut self, bundle: &[(&str, &str)]) {
        let files: BTreeMap<&str, &str> = bundle.iter().copied().collect();
        let mut manifests: Vec<(&str, &str)> = files
            .iter()
            .filter_map(|(path, contents)| {
                let mut parts = path.split('/');
                let (Some("skills"), Some(dir), Some("skill.toml"), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return None;
                };
                (!dir.starts_with('.')).then_some((dir, *contents))
            })
            .collect();
        manifests.sort_by_key(|(dir, _)| *dir);
        let mut bundled: BTreeMap<String, SkillIndexEntry> = BTreeMap::new();
        for (dir, source) in manifests {
            let location = format!("{BUNDLED_SKILL_SOURCE}:skills/{dir}");
            let manifest: SkillToml = match toml::from_str(source) {
                Ok(manifest) => manifest,
                Err(error) => {
                    self.skipped.push(SkippedSkill {
                        location,
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            if self.entries.contains_key(&manifest.name) || self.disabled.contains(&manifest.name) {
                continue;
            }
            if !manifest.enabled {
                bundled.remove(&manifest.name);
                continue;
            }
            let filename = manifest
                .instructions_file
                .as_deref()
                .unwrap_or("instructions.md");
            if filename.contains("..") || filename.starts_with('/') {
                self.skipped.push(SkippedSkill {
                    location,
                    reason: format!("unzulässiger instructions_file {filename:?}"),
                });
                continue;
            }
            let relative = format!("skills/{dir}/{filename}");
            let instructions = files.get(relative.as_str()).copied().unwrap_or_default();
            if instructions.len() > MAX_INSTRUCTIONS_BYTES {
                self.skipped.push(SkippedSkill {
                    location,
                    reason: format!("Anweisungstext größer als {MAX_INSTRUCTIONS_BYTES} Bytes"),
                });
                continue;
            }
            let trigger_source = files
                .get(format!("skills/{dir}/{TRIGGERS_FILE}").as_str())
                .map(|text| {
                    if text.len() > MAX_TRIGGERS_FILE_BYTES {
                        Err(format!(
                            "{TRIGGERS_FILE} größer als {MAX_TRIGGERS_FILE_BYTES} Bytes, verworfen"
                        ))
                    } else {
                        Ok((*text).to_owned())
                    }
                });
            let spec = self.trigger_spec(&manifest.name, trigger_source);
            let snapshot = SkillRuntimeSnapshot {
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                instructions: instructions.to_owned(),
                tools: manifest.tools.clone(),
                mcps: manifest.mcps.clone(),
                source_path: PathBuf::from(format!("{BUNDLED_SKILL_SOURCE}:{relative}")),
                sha256: sha256_hex(instructions.as_bytes()),
            };
            bundled.insert(
                manifest.name.clone(),
                SkillIndexEntry::new(snapshot, SkillSource::Bundled, spec),
            );
        }
        self.entries.extend(bundled);
    }

    /// Anzahl der verfügbaren (aktivierten) Skills.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Ob der Index leer ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Alle Einträge, alphabetisch nach Name.
    pub fn entries(&self) -> impl Iterator<Item = &SkillIndexEntry> {
        self.entries.values()
    }

    /// Der Eintrag `name`: exakt, sonst ohne Groß-/Kleinschreibung.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SkillIndexEntry> {
        let name = name.trim();
        self.entries.get(name).or_else(|| {
            self.entries
                .values()
                .find(|entry| entry.name.eq_ignore_ascii_case(name))
        })
    }

    /// Ob `name` in einem Layer ausdrücklich deaktiviert ist.
    #[must_use]
    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.contains(name)
    }

    /// Beim Bau übersprungene Skills.
    #[must_use]
    pub fn skipped(&self) -> &[SkippedSkill] {
        &self.skipped
    }

    /// Warnungen zu verworfenen Triggern (der Skill selbst ist geladen).
    #[must_use]
    pub fn warnings(&self) -> &[SkillWarning] {
        &self.warnings
    }

    /// Der einmal beim Bau erstellte, unveränderliche Trigger-Index.
    #[must_use]
    pub fn trigger_index(&self) -> &TriggerIndex {
        &self.triggers
    }

    /// Rangiert die Skills gegen `query`.
    ///
    /// # Beschreibung
    /// Einfache Token-Suche ohne Groß-/Kleinschreibung (Umlaute gefaltet,
    /// `ä` = `ae`): je Suchwort zählen Treffer im Namen am stärksten, dann in
    /// der Beschreibung, dann in den Überschriften des Anweisungstexts.
    /// Exakte Wortgleichheit wiegt mehr als ein gemeinsamer Wortanfang
    /// (≥ 4 Zeichen, fängt Beugungen wie „Pyramide“/„pyramid“), dieser mehr
    /// als eine Teilzeichenkette (≥ 4 Zeichen, fängt Komposita). Jedes
    /// getroffene Suchwort gibt zusätzlich einen Bonus, damit Skills, die
    /// alle Wörter treffen, vorn stehen. Füllwörter werden ignoriert.
    ///
    /// Eine leere (oder nur aus Füllwörtern bestehende) Anfrage liefert alle
    /// Skills alphabetisch mit Score 0.
    ///
    /// # Rückgabe
    /// Alle Treffer mit Score > 0, absteigend nach Score, bei Gleichstand
    /// alphabetisch.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<SkillSearchHit<'_>> {
        let words: Vec<String> = tokens(query)
            .into_iter()
            .filter(|word| !STOP_WORDS.contains(&word.as_str()))
            .collect();
        if words.is_empty() {
            return self
                .entries
                .values()
                .map(|entry| SkillSearchHit { entry, score: 0 })
                .collect();
        }
        let mut hits: Vec<SkillSearchHit<'_>> = self
            .entries
            .values()
            .filter_map(|entry| {
                let score = score_entry(entry, &words);
                (score > 0).then_some(SkillSearchHit { entry, score })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.entry.name.cmp(&b.entry.name))
        });
        hits
    }

    /// Die bis zu `limit` Skill-Namen, die `name` am ähnlichsten sind
    /// (Teilzeichenkette zuerst, dann Editierdistanz, dann Suchrelevanz).
    #[must_use]
    pub fn closest_names(&self, name: &str, limit: usize) -> Vec<&str> {
        let wanted = fold(name.trim());
        let relevance: BTreeMap<&str, u32> = self
            .search(name)
            .into_iter()
            .map(|hit| (hit.entry.name.as_str(), hit.score))
            .collect();
        let mut ranked: Vec<(bool, usize, u32, &str)> = self
            .entries
            .values()
            .map(|entry| {
                let candidate = fold(&entry.name);
                let contains = !wanted.is_empty()
                    && (candidate.contains(&wanted) || wanted.contains(&candidate));
                (
                    !contains,
                    edit_distance(&wanted, &candidate),
                    u32::MAX - relevance.get(entry.name.as_str()).copied().unwrap_or(0),
                    entry.name.as_str(),
                )
            })
            .collect();
        ranked.sort();
        ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, _, name)| name)
            .collect()
    }
}

/// Füllwörter, die bei der Suche nicht zählen (deutsch/englisch).
const STOP_WORDS: &[&str] = &[
    "der", "die", "das", "den", "dem", "des", "ein", "eine", "einen", "und", "oder", "fuer", "mit",
    "zu", "zum", "zur", "es", "gibt", "welche", "welcher", "was", "wie", "im", "in", "am", "an",
    "auf", "von", "ist", "sind", "ich", "wir", "skill", "skills", "the", "and", "or", "for", "to",
    "of", "is", "with",
];

/// Faltet Groß-/Kleinschreibung und deutsche Umlaute.
pub(crate) fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars().flat_map(char::to_lowercase) {
        match ch {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            other => out.push(other),
        }
    }
    out
}

/// Zerlegt `text` in gefaltete Wörter aus Buchstaben/Ziffern (≥ 2 Zeichen).
pub(crate) fn tokens(text: &str) -> Vec<String> {
    fold(text)
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_owned)
        .collect()
}

/// Wie gut `word` zu einem Token `token` passt: 3 exakt, 2 gemeinsamer
/// Wortanfang (≥ 4 Zeichen), 1 Teilzeichenkette (≥ 4 Zeichen), sonst 0.
fn match_strength(word: &str, token: &str) -> u32 {
    if word == token {
        return 3;
    }
    let shorter = word.chars().count().min(token.chars().count());
    if shorter >= 4 && (token.starts_with(word) || word.starts_with(token)) {
        return 2;
    }
    if word.chars().count() >= 4 && token.contains(word) {
        return 1;
    }
    0
}

/// Beste Trefferstärke von `word` in `field_tokens`.
fn best_match(word: &str, field_tokens: &[String]) -> u32 {
    field_tokens
        .iter()
        .map(|token| match_strength(word, token))
        .max()
        .unwrap_or(0)
}

/// Gewicht eines Namens-, Beschreibungs- bzw. Überschriftentreffers.
const NAME_WEIGHT: u32 = 10;
const DESCRIPTION_WEIGHT: u32 = 4;
const HEADING_WEIGHT: u32 = 2;
/// Bonus je getroffenem Suchwort.
const WORD_BONUS: u32 = 5;

/// Score eines Eintrags gegen die (gefalteten, gefilterten) Suchwörter.
pub(crate) fn score_entry(entry: &SkillIndexEntry, words: &[String]) -> u32 {
    let name_tokens = tokens(&entry.name);
    let description_tokens = tokens(&entry.description);
    let heading_tokens: Vec<String> = entry
        .headings
        .iter()
        .flat_map(|heading| tokens(&heading.text))
        .collect();
    let mut score = 0;
    for word in words {
        let word_score = NAME_WEIGHT * best_match(word, &name_tokens)
            + DESCRIPTION_WEIGHT * best_match(word, &description_tokens)
            + HEADING_WEIGHT * best_match(word, &heading_tokens);
        if word_score > 0 {
            score += word_score + WORD_BONUS;
        }
    }
    score
}

/// Liest `triggers.toml` eines Skill-Verzeichnisses. `None`, wenn es fehlt.
/// Größenprüfung vor dem Lesen; Symlink-/Traversal-Schutz über
/// [`harw_config::load_skill_instructions`] wie bei `instructions_file`.
fn read_layer_triggers(dir: &Path) -> Option<Result<String, String>> {
    let path = dir.join(TRIGGERS_FILE);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => return Some(Err(format!("{TRIGGERS_FILE} nicht lesbar: {error}"))),
    };
    if !metadata.is_file() {
        return Some(Err(format!("{TRIGGERS_FILE} ist keine Datei, verworfen")));
    }
    if metadata.len() > MAX_TRIGGERS_FILE_BYTES as u64 {
        return Some(Err(format!(
            "{TRIGGERS_FILE} größer als {MAX_TRIGGERS_FILE_BYTES} Bytes, verworfen"
        )));
    }
    Some(
        harw_config::load_skill_instructions(dir, Some(TRIGGERS_FILE))
            .map_err(|error| format!("{TRIGGERS_FILE} nicht lesbar: {error}")),
    )
}

/// Levenshtein-Distanz über Unicode-Zeichen.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Die Überschriften von `text` außerhalb von Codeblöcken als
/// `(Byte-Offset des Zeilenanfangs, Ebene, Text)`.
pub(crate) fn markdown_headings(text: &str) -> Vec<(usize, usize, String)> {
    let mut headings = Vec::new();
    let mut in_fence = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") || trimmed.trim_start().starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !trimmed.starts_with('#') {
            continue;
        }
        let level = trimmed.chars().take_while(|ch| *ch == '#').count();
        let rest = &trimmed[level..];
        if level > 6 || !(rest.is_empty() || rest.starts_with(' ')) {
            continue;
        }
        let heading = rest.trim().trim_end_matches('#').trim();
        if !heading.is_empty() {
            headings.push((start, level, heading.to_owned()));
        }
    }
    headings
}

/// Der erste Satz von `text` (bis zum ersten `.`, `!` oder `?` vor
/// Leerraum bzw. Textende), auf `max_chars` Zeichen gekürzt.
pub(crate) fn first_sentence(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    let mut end = text.len();
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if matches!(ch, '.' | '!' | '?') {
            let next_is_space = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
            if next_is_space {
                end = index + ch.len_utf8();
                break;
            }
        }
    }
    let sentence = &text[..end];
    match sentence.char_indices().nth(max_chars) {
        None => sentence.to_owned(),
        Some((cut, _)) => format!("{}…", sentence[..cut].trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn write_skill(
        layer: &Path,
        dir: &str,
        manifest: &str,
        instructions: &str,
    ) -> TestResult<PathBuf> {
        let skill_dir = layer.join("skills").join(dir);
        std::fs::create_dir_all(&skill_dir).map_err(ctx("mkdir skill"))?;
        std::fs::write(skill_dir.join("skill.toml"), manifest).map_err(ctx("write manifest"))?;
        std::fs::write(skill_dir.join("instructions.md"), instructions)
            .map_err(ctx("write instructions"))?;
        Ok(skill_dir)
    }

    const BODY: &str = "# Titel\n\nEinleitung.\n\n## Wann nutzen\n\nImmer.\n\n```\n## kein Abschnitt\n```\n\n## Schritte\n\n### Detail\n\nText.\n\n## Checkliste\n\n- a\n";

    #[test]
    fn bundle_fallback_works_with_an_empty_home() -> TestResult {
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let index = SkillIndex::build(&[empty.path().to_path_buf()]);
        assert!(
            index.len() >= 50,
            "alle eingebetteten Skills stehen ohne Scaffold bereit: {}",
            index.len()
        );
        let entry = index
            .get("business-writing-pyramid")
            .ok_or(TestError::Missing("business-writing-pyramid"))?;
        assert_eq!(entry.source, SkillSource::Bundled);
        assert_eq!(entry.source.label(), BUNDLED_SKILL_SOURCE);
        assert!(entry.size_bytes > 0);
        assert!(
            entry
                .fragment()
                .starts_with("# Skill: business-writing-pyramid (sha256 ")
        );
        assert!(index.skipped().is_empty(), "{:?}", index.skipped());
        // Auch ganz ohne Layer.
        assert_eq!(SkillIndex::build(&[]).len(), index.len());
        Ok(())
    }

    #[test]
    fn search_finds_the_pyramid_skill_for_pyramide_and_pyramidenprinzip() -> TestResult {
        let index = SkillIndex::build(&[]);
        for query in ["Pyramide", "Pyramidenprinzip", "pyramide Kernaussage"] {
            let hits = index.search(query);
            let first = hits
                .first()
                .ok_or(TestError::Unexpected(format!("{query}: kein Treffer")))?;
            assert_eq!(first.entry.name, "business-writing-pyramid", "{query}");
        }
        Ok(())
    }

    #[test]
    fn business_paper_section_of_latex_report_loads_under_the_new_heading() -> TestResult {
        let index = SkillIndex::build(&[]);
        let entry = index
            .get("latex-report")
            .ok_or(TestError::Missing("latex-report"))?;
        // Teilzeichenkette: „business-paper“ trifft die ganze Überschrift.
        let section = entry
            .section_fragment("business-paper")
            .ok_or(TestError::Missing("Abschnitt business-paper"))?;
        assert!(
            section.contains("### business-paper (Pyramidenprinzip)"),
            "{section}"
        );
        assert!(section.contains("`business-writing-pyramid`"), "{section}");
        assert!(!section.contains("### handbuch"), "{section}");
        Ok(())
    }

    #[test]
    fn empty_query_lists_everything_alphabetically() {
        let index = SkillIndex::build(&[]);
        let names: Vec<&str> = index
            .search("  ")
            .iter()
            .map(|hit| hit.entry.name.as_str())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), index.len());
        // Nur Füllwörter zählen wie eine leere Anfrage.
        assert_eq!(index.search("welche skills gibt es").len(), index.len());
    }

    #[test]
    fn layer_skills_win_over_the_bundle_and_disabled_ones_hide_it() -> TestResult {
        let low = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let high = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_skill(
            low.path(),
            "a",
            "name = \"eigener\"\ndescription = \"Niedrig.\"\n",
            BODY,
        )?;
        let winner = write_skill(
            high.path(),
            "b",
            "name = \"eigener\"\ndescription = \"Hoch. Zweiter Satz.\"\n",
            BODY,
        )?;
        write_skill(
            high.path(),
            "pyr",
            "name = \"business-writing-pyramid\"\nenabled = false\n",
            "",
        )?;
        let bundle = [
            (
                "skills/x/skill.toml",
                "name = \"eigener\"\ndescription = \"Bündel\"\n",
            ),
            ("skills/x/instructions.md", "Bündeltext"),
            (
                "skills/business-writing-pyramid/skill.toml",
                "name = \"business-writing-pyramid\"\n",
            ),
            ("skills/only/skill.toml", "name = \"nur-im-buendel\"\n"),
        ];
        let index = SkillIndex::build_with_bundle(
            &[low.path().to_path_buf(), high.path().to_path_buf()],
            &bundle,
        );
        let own = index.get("eigener").ok_or(TestError::Missing("eigener"))?;
        assert_eq!(own.source, SkillSource::Layer(winner));
        assert_eq!(own.short_description(), "Hoch.");
        assert!(index.get("business-writing-pyramid").is_none());
        assert!(index.is_disabled("business-writing-pyramid"));
        let only = index
            .get("NUR-IM-BUENDEL")
            .ok_or(TestError::Missing("nur-im-buendel"))?;
        assert_eq!(only.source, SkillSource::Bundled);
        assert_eq!(only.size_bytes, 0);
        assert_eq!(index.len(), 2);
        Ok(())
    }

    #[test]
    fn sections_are_listed_and_loaded_individually() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_skill(layer.path(), "s", "name = \"sektionen\"\n", BODY)?;
        let index = SkillIndex::build_with_bundle(&[layer.path().to_path_buf()], &[]);
        let entry = index
            .get("sektionen")
            .ok_or(TestError::Missing("sektionen"))?;
        assert_eq!(entry.sections(), ["Wann nutzen", "Schritte", "Checkliste"]);

        let steps = entry
            .section_fragment("schritte")
            .ok_or(TestError::Missing("Abschnitt Schritte"))?;
        assert!(steps.starts_with("# Skill: sektionen (sha256 "), "{steps}");
        assert!(steps.contains("## Schritte"));
        assert!(steps.contains("### Detail"), "Unterabschnitte gehören dazu");
        assert!(!steps.contains("Checkliste"), "{steps}");
        assert!(!steps.contains("Immer."), "{steps}");

        // Teilzeichenkette und `##`-Präfix werden akzeptiert.
        let check = entry
            .section_fragment("## Check")
            .ok_or(TestError::Missing("Abschnitt Checkliste"))?;
        assert!(check.contains("- a"));
        // Überschriften in Codeblöcken sind keine Abschnitte.
        assert!(entry.section_fragment("kein Abschnitt").is_none());
        assert!(entry.section_fragment("gibt es nicht").is_none());
        Ok(())
    }

    #[test]
    fn closest_names_prefer_substrings_then_edit_distance() {
        let index = SkillIndex::build(&[]);
        let closest = index.closest_names("business-writing", 5);
        assert_eq!(closest.len(), 5);
        assert_eq!(closest.first().copied(), Some("business-writing-pyramid"));
        let typo = index.closest_names("latex-reprot", 5);
        assert!(typo.contains(&"latex-report"), "{typo:?}");
    }

    #[test]
    fn first_sentence_is_cut_at_the_limit() {
        assert_eq!(first_sentence("Eins. Zwei.", 160), "Eins.");
        assert_eq!(first_sentence("v1.2 ist gut", 160), "v1.2 ist gut");
        let long = "x".repeat(200);
        let cut = first_sentence(&long, 160);
        assert_eq!(cut.chars().count(), 161);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn edit_distance_counts_single_edits() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("same", "same"), 0);
    }
}
