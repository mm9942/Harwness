//! Bundle-Prüfungen der Analyse-Familie (Plan R9, Teil D).
//!
//! Die zehn Agenten der Familie (`agents/<name>/definition.toml` +
//! `system.md`) und ihre Methoden-Skills werden mit dem Binary ausgeliefert.
//! Diese Tests sichern vier Zusagen:
//!
//! 1. Jede mitgelieferte `definition.toml` parst als
//!    [`harw_agent_dsl::raw::RawAgentDefinition`]; die Familie hält ihre
//!    Schema-Konventionen ein (ID, Basis, Tiefe, Instruktionsdatei).
//! 2. Jeder Skill, auf den eine mitgelieferte Definition (`definition.toml`
//!    oder Legacy-`agent.toml`) verweist, wird mitgeliefert.
//! 3. Die Methoden-Skills haben den verabredeten Aufbau und eine Größe, die
//!    `skills.load` ohne Abschnittsnachladen liefert.
//! 4. Kein mitgelieferter Text nennt Buchtitel, Autoren oder Herausgeber aus
//!    dem Quellmaterial der Familie — die Inhalte sind neutral formulierte
//!    Arbeitsanleitungen. Die Sperrliste steht klein geschrieben hier und
//!    wird unabhängig von Groß-/Kleinschreibung an Wortgrenzen geprüft;
//!    Allerweltswörter (Vornamen, gängige Nachnamen, deutsche Wörter) stehen
//!    bewusst nicht darauf, sondern nur spezifische Namen und Titel.
//!
//! Ausschließlich öffentliche API (`harw_home::bundled_files`).

use harw_agent_dsl::roles::AgentRoleId;
use harw_home::bundled_files;

/// Ergebnis eines Tests: ein Fehlschlag wird als `Err` gemeldet statt zu paniken.
type TestResult = Result<(), String>;

/// Die zehn Agenten der Analyse-Familie.
const FAMILY_AGENTS: &[&str] = &[
    "evidence-collector",
    "scenario-player",
    "evidence-critic",
    "systems-modeller",
    "pattern-analyst",
    "method-auditor",
    "synthesis-writer",
    "intel-analysis-orchestrator",
    "wargaming-orchestrator",
    "evidence-review-orchestrator",
];

/// Die Orchestratoren unter den [`FAMILY_AGENTS`].
const FAMILY_ORCHESTRATORS: &[&str] = &[
    "intel-analysis-orchestrator",
    "wargaming-orchestrator",
    "evidence-review-orchestrator",
];

/// Die Methoden-Skills der Familie.
const FAMILY_SKILLS: &[&str] = &[
    "competing-hypotheses",
    "key-assumptions-check",
    "evidence-quality-review",
    "confidence-and-uncertainty",
    "feedback-loops-and-thresholds",
    "scenario-wargaming",
    "link-and-pattern-analysis",
    "premortem-and-red-team",
    "method-validation",
    "analysis-workflow",
    // Plan R9, Web-Recherche mit Quellen (`intel-web-researcher`).
    "osint-web-research",
];

/// Plan R9: der Web-Rechercheur der Familie. Er setzt nicht auf
/// `worker-base`, sondern auf die eingebaute Rolle `researcher-web` auf und
/// erbt deren Rechtedecke (Netz nur egress-gebunden, kein Workspace).
const WEB_RESEARCHER: &str = "intel-web-researcher";

/// Die Skills, die der Web-Rechercheur bei Bedarf lädt (fest gebunden ist nur
/// `evidence-quality-review`).
const WEB_RESEARCHER_SKILLS: &[&str] = &[
    "evidence-quality-review",
    "osint-web-research",
    "competing-hypotheses",
    "confidence-and-uncertainty",
    "key-assumptions-check",
];

/// Pflichtabschnitte jedes Methoden-Skills (`skills.search` findet den Skill
/// über die Beschreibung, der Agent entscheidet über „Wann nutzen“).
const SKILL_SECTIONS: &[&str] = &[
    "## Wann nutzen",
    "## Vorgehen",
    "## Ergebnisformat",
    "## Fallstricke",
];

/// Namen und Titel aus dem Quellmaterial, die in keinem ausgelieferten Text
/// vorkommen dürfen (klein geschrieben, Wortgrenzen-Treffer).
const BLOCKLIST: &[&str] = &[
    // Autoren und Herausgeber
    "parkhurst",
    "volchenkov",
    "prunckun",
    "oriesek",
    "citrenbaum",
    "bar-joseph",
    "mcdermott",
    "ronczkowski",
    "mcglynn",
    "fischhoff",
    "tetlock",
    "lowenthal",
    "heuer",
    "milne",
    "treverton",
    "maceachin",
    "wohlstetter",
    "edmondson",
    "miller/page",
    "miller and page",
    "george/bruce",
    "george and bruce",
    "david t. moore",
    "sherman kent",
    "wise/towne",
    "nrc",
    "national research council",
    "skram",
    "analytical olympics",
    // Buchtitel
    "politics of evidence",
    "business wargaming",
    "complex adaptive systems",
    "survival under uncertainty",
    "analyzing intelligence",
    "intelligence analysis for tomorrow",
    "intelligence analysis fundamentals",
    "forensic intelligence",
    "critical thinking and intelligence analysis",
    "critical thinking for intelligence analysts",
    "intelligence success and failure",
    "scientific methods of inquiry",
    "advancing scientific research",
    "how to think in complex environments",
];

/// Liefert den Inhalt der Bundle-Datei `relative_path`.
fn bundled(relative_path: &str) -> Result<&'static str, String> {
    bundled_files()
        .iter()
        .find(|file| file.relative_path == relative_path)
        .map(|file| file.contents)
        .ok_or_else(|| format!("{relative_path} fehlt im Bundle"))
}

/// Namen aller mitgelieferten Skills (`skills/<name>/skill.toml`).
fn bundled_skill_names() -> Vec<&'static str> {
    bundled_files()
        .iter()
        .filter_map(|file| {
            file.relative_path
                .strip_prefix("skills/")
                .and_then(|rest| rest.strip_suffix("/skill.toml"))
        })
        .collect()
}

/// Namen aller mitgelieferten DSL-Definitionen (`agents/<name>/definition.toml`).
fn bundled_definition_names() -> Vec<&'static str> {
    bundled_files()
        .iter()
        .filter_map(|file| {
            file.relative_path
                .strip_prefix("agents/")
                .and_then(|rest| rest.strip_suffix("/definition.toml"))
        })
        .collect()
}

/// `true`, wenn `needle` in `haystack` an Wortgrenzen vorkommt (beide klein
/// geschrieben). Wortgrenze heißt: davor und danach kein Buchstabe und keine
/// Ziffer — so trifft `nrc` nicht mitten in einem längeren Wort.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Liest die `[delegation].targets` einer Definition.
fn delegation_targets(raw: &harw_agent_dsl::raw::RawAgentDefinition) -> Vec<String> {
    raw.tables
        .get("delegation")
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("targets"))
        .and_then(toml::Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn test_every_bundled_definition_parses_as_raw_agent_definition() -> TestResult {
    let mut parsed = 0;
    for file in bundled_files() {
        if !file.relative_path.ends_with("/definition.toml") {
            continue;
        }
        harw_agent_dsl::parse::parse_toml(file.contents)
            .map_err(|error| format!("{}: {error}", file.relative_path))?;
        parsed += 1;
    }
    assert!(
        parsed >= FAMILY_AGENTS.len(),
        "zu wenige Definitionen im Bundle: {parsed} statt mindestens {}",
        FAMILY_AGENTS.len()
    );
    Ok(())
}

#[test]
fn test_family_definitions_follow_the_schema_conventions() -> TestResult {
    for name in FAMILY_AGENTS {
        let path = format!("agents/{name}/definition.toml");
        let raw = harw_agent_dsl::parse::parse_toml(bundled(&path)?)
            .map_err(|error| format!("{path}: {error}"))?;

        assert_eq!(raw.schema, "harwness.agent/v1", "{path}: schema");
        assert_eq!(
            raw.id.as_string(),
            format!("harwness.agent.{name}@1"),
            "{path}: id"
        );
        assert_eq!(raw.specialization, *name, "{path}: specialization");
        assert!(raw.name.is_some(), "{path}: name fehlt");
        assert!(
            raw.description
                .as_deref()
                .is_some_and(|d| !d.trim().is_empty()),
            "{path}: description fehlt"
        );

        // Die Instruktionsdatei liegt neben der Definition und wird mitgeliefert.
        let instructions = raw
            .tables
            .get("instructions_file")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("{path}: instructions_file fehlt"))?;
        assert_eq!(instructions, "system.md", "{path}: instructions_file");
        let system = bundled(&format!("agents/{name}/system.md"))?;
        assert!(
            system.contains("## Was ich NICHT tue"),
            "agents/{name}/system.md: Abschnitt „Was ich NICHT tue“ fehlt"
        );

        let base = raw
            .extends
            .as_ref()
            .map(|reference| reference.id.as_string())
            .ok_or_else(|| format!("{path}: extends fehlt"))?;
        if FAMILY_ORCHESTRATORS.contains(name) {
            assert_eq!(
                base, "harwness.agent.child-orchestrator-base@1",
                "{path}: Orchestratoren setzen auf child-orchestrator-base auf"
            );
            assert_eq!(raw.role, AgentRoleId::ChildOrchestrator, "{path}: role");
            // Tiefe, Budget, Werkzeuge und Rückgabe besitzt die Basis; eigene
            // Sektionen gleichen Namens würden beim Auflösen verworfen.
            for owned_by_base in ["spawn", "tools", "return", "work", "lifecycle", "context"] {
                assert!(
                    !raw.tables.contains_key(owned_by_base),
                    "{path}: [{owned_by_base}] gehört der Basis und würde verworfen"
                );
            }
            assert!(
                !delegation_targets(&raw).is_empty(),
                "{path}: [delegation].targets fehlt"
            );
        } else {
            assert_eq!(
                base, "harwness.agent.worker-base@1",
                "{path}: Worker setzen auf worker-base auf"
            );
            assert_eq!(raw.role, AgentRoleId::Worker, "{path}: role");
            let max_depth = raw
                .tables
                .get("spawn")
                .and_then(toml::Value::as_table)
                .and_then(|spawn| spawn.get("max_depth"))
                .and_then(toml::Value::as_integer);
            assert_eq!(max_depth, Some(0), "{path}: Worker starten keine Kinder");
            let contract = raw
                .tables
                .get("return")
                .and_then(toml::Value::as_table)
                .and_then(|table| table.get("contract"))
                .and_then(toml::Value::as_str);
            assert_eq!(
                contract,
                Some("harwness.return.execution-summary@1"),
                "{path}: [return].contract"
            );
        }
        assert!(
            raw.skills.len() <= 1,
            "{path}: höchstens ein fester Skill; der Rest kommt über skills.load"
        );
    }
    Ok(())
}

#[test]
fn test_family_delegation_targets_are_bundled_family_members() -> TestResult {
    let definitions = bundled_definition_names();
    for name in FAMILY_ORCHESTRATORS {
        let path = format!("agents/{name}/definition.toml");
        let raw = harw_agent_dsl::parse::parse_toml(bundled(&path)?)
            .map_err(|error| format!("{path}: {error}"))?;
        for target in delegation_targets(&raw) {
            assert!(
                definitions.contains(&target.as_str()),
                "{path}: Delegationsziel '{target}' wird nicht mitgeliefert"
            );
            assert!(
                !FAMILY_ORCHESTRATORS.contains(&target.as_str()),
                "{path}: '{target}' ist ein Orchestrator — max_depth = 1 erlaubt nur Worker"
            );
        }
    }
    Ok(())
}

#[test]
fn test_only_the_synthesis_writer_admits_write_tools() -> TestResult {
    for name in FAMILY_AGENTS {
        let path = format!("agents/{name}/definition.toml");
        let raw = harw_agent_dsl::parse::parse_toml(bundled(&path)?)
            .map_err(|error| format!("{path}: {error}"))?;
        let admitted: Vec<&str> = raw
            .tables
            .get("tools")
            .and_then(toml::Value::as_table)
            .and_then(|tools| tools.get("admitted"))
            .and_then(toml::Value::as_array)
            .map(|list| list.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();
        let writes = admitted
            .iter()
            .any(|tool| *tool == "fs.write" || *tool == "fs.edit");
        assert_eq!(
            writes,
            *name == "synthesis-writer",
            "{path}: nur synthesis-writer darf schreiben"
        );
        assert!(
            !admitted.contains(&"shell.exec"),
            "{path}: kein Mitglied der Familie führt Prozesse aus"
        );
    }
    Ok(())
}

/// Ein Agent, der auf einen nicht mitgelieferten Skill verweist, wäre beim
/// Nutzer sofort kaputt — gilt für DSL-Definitionen und Legacy-`agent.toml`.
#[test]
fn test_every_skill_referenced_by_a_bundled_definition_ships() -> TestResult {
    let available = bundled_skill_names();
    for file in bundled_files() {
        let referenced: Vec<String> = if file.relative_path.ends_with("/definition.toml") {
            harw_agent_dsl::parse::parse_toml(file.contents)
                .map_err(|error| format!("{}: {error}", file.relative_path))?
                .skills
        } else if file.relative_path.ends_with("/agent.toml") {
            let value: toml::Table = toml::from_str(file.contents)
                .map_err(|error| format!("{}: {error}", file.relative_path))?;
            value
                .get("skills")
                .and_then(toml::Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|skill| skill.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            continue;
        };
        for skill in referenced {
            assert!(
                available.contains(&skill.as_str()),
                "{} verweist auf den fehlenden Skill '{skill}'",
                file.relative_path
            );
        }
    }
    Ok(())
}

/// Die Skills, die die Arbeitsanweisungen der Familie per `skills.load`
/// nennen, werden ebenfalls mitgeliefert.
#[test]
fn test_skills_named_in_family_instructions_ship() -> TestResult {
    let available = bundled_skill_names();
    let expected_mentions: &[(&str, &[&str])] = &[
        (
            "synthesis-writer",
            &["business-writing-pyramid", "confidence-and-uncertainty"],
        ),
        (
            "pattern-analyst",
            &["competing-hypotheses", "link-and-pattern-analysis"],
        ),
        (
            "evidence-critic",
            &["key-assumptions-check", "evidence-quality-review"],
        ),
        (
            "scenario-player",
            &["scenario-wargaming", "premortem-and-red-team"],
        ),
        (
            "method-auditor",
            &["method-validation", "confidence-and-uncertainty"],
        ),
        ("systems-modeller", &["feedback-loops-and-thresholds"]),
        ("intel-analysis-orchestrator", &["analysis-workflow"]),
        ("wargaming-orchestrator", &["scenario-wargaming"]),
        (
            "evidence-review-orchestrator",
            &["analysis-workflow", "method-validation"],
        ),
    ];
    for (agent, skills) in expected_mentions {
        let system = bundled(&format!("agents/{agent}/system.md"))?;
        for skill in *skills {
            assert!(
                available.contains(skill),
                "{agent}: Skill '{skill}' wird nicht mitgeliefert"
            );
            assert!(
                system.contains(&format!("`{skill}`")),
                "agents/{agent}/system.md nennt '{skill}' nicht"
            );
        }
    }
    Ok(())
}

#[test]
fn test_family_skills_have_the_agreed_structure_and_size() -> TestResult {
    for name in FAMILY_SKILLS {
        let manifest_path = format!("skills/{name}/skill.toml");
        let manifest: toml::Table = toml::from_str(bundled(&manifest_path)?)
            .map_err(|error| format!("{manifest_path}: {error}"))?;
        assert_eq!(
            manifest.get("name").and_then(toml::Value::as_str),
            Some(*name),
            "{manifest_path}: name"
        );
        assert_eq!(
            manifest
                .get("instructions_file")
                .and_then(toml::Value::as_str),
            Some("instructions.md"),
            "{manifest_path}: instructions_file"
        );
        let description = manifest
            .get("description")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        assert!(
            description.len() >= 80,
            "{manifest_path}: description ist zu dünn für skills.search"
        );

        let instructions_path = format!("skills/{name}/instructions.md");
        let instructions = bundled(&instructions_path)?;
        for section in SKILL_SECTIONS {
            assert!(
                instructions.contains(section),
                "{instructions_path}: Abschnitt '{section}' fehlt"
            );
        }
        assert!(
            (3_000..=8_500).contains(&instructions.len()),
            "{instructions_path}: {} Bytes, verabredet sind etwa 3–8 KB",
            instructions.len()
        );
    }
    Ok(())
}

#[test]
fn test_no_bundled_file_names_source_books_or_authors() -> TestResult {
    let mut hits = Vec::new();
    for file in bundled_files() {
        let text = file.contents.to_lowercase();
        for name in BLOCKLIST {
            if contains_word(&text, name) {
                hits.push(format!("{}: '{name}'", file.relative_path));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "ausgelieferte Texte nennen Quellmaterial: {hits:?}"
    );
    Ok(())
}

#[test]
fn test_family_assets_carry_no_source_lines() -> TestResult {
    for file in bundled_files() {
        let in_family = FAMILY_AGENTS
            .iter()
            .chain(std::iter::once(&WEB_RESEARCHER))
            .any(|name| file.relative_path.starts_with(&format!("agents/{name}/")))
            || FAMILY_SKILLS
                .iter()
                .any(|name| file.relative_path.starts_with(&format!("skills/{name}/")));
        if !in_family {
            continue;
        }
        for line in file.contents.lines() {
            let trimmed =
                line.trim_start_matches(|c: char| c.is_whitespace() || c == '>' || c == '*');
            assert!(
                !trimmed.starts_with("Quelle:") && !trimmed.starts_with("Quellen:"),
                "{}: Quellenzeile '{line}'",
                file.relative_path
            );
        }
    }
    Ok(())
}

/// Plan R9: `intel-web-researcher` validiert, erweitert `researcher-web`,
/// bindet fest nur `evidence-quality-review` und überlässt Werkzeuge, Tiefe,
/// Budget und Rückgabe der Basis (eigene Tabellen würden verworfen).
#[test]
fn test_web_researcher_extends_researcher_web_and_owns_no_rights() -> TestResult {
    let path = format!("agents/{WEB_RESEARCHER}/definition.toml");
    let raw = harw_agent_dsl::parse::parse_toml(bundled(&path)?)
        .map_err(|error| format!("{path}: {error}"))?;
    assert_eq!(raw.schema, "harwness.agent/v1", "{path}: schema");
    assert_eq!(
        raw.id.as_string(),
        format!("harwness.agent.{WEB_RESEARCHER}@1"),
        "{path}: id"
    );
    assert_eq!(raw.specialization, WEB_RESEARCHER, "{path}: specialization");
    assert_eq!(raw.role, AgentRoleId::Worker, "{path}: role");
    let base = raw
        .extends
        .as_ref()
        .map(|reference| reference.id.as_string())
        .ok_or_else(|| format!("{path}: extends fehlt"))?;
    assert_eq!(base, "harwness.agent.researcher-web@1", "{path}: Basis");
    assert_eq!(
        raw.skills,
        ["evidence-quality-review"],
        "{path}: fester Skill"
    );
    for owned_by_base in ["tools", "spawn", "return", "context", "research"] {
        assert!(
            !raw.tables.contains_key(owned_by_base),
            "{path}: [{owned_by_base}] gehört der Basis researcher-web"
        );
    }
    let description = raw.description.as_deref().unwrap_or_default();
    for needle in ["Primärquellen", "URL", "Abrufdatum", "Evidenzpaket"] {
        assert!(
            description.contains(needle),
            "{path}: description ohne {needle}"
        );
    }
    assert_eq!(
        raw.tables
            .get("instructions_file")
            .and_then(toml::Value::as_str),
        Some("system.md"),
        "{path}: instructions_file"
    );
    Ok(())
}

/// Plan R9: die Arbeitsanweisung nennt die Methoden-Skills, die mitgeliefert
/// werden, das Evidenzpaket mit URL, Abrufdatum und Einstufung, und die
/// Grenzen (keine Personendaten, keine Logins, keine Paywall-Umgehung).
#[test]
fn test_web_researcher_instructions_name_skills_packet_and_limits() -> TestResult {
    let available = bundled_skill_names();
    let system = bundled(&format!("agents/{WEB_RESEARCHER}/system.md"))?;
    for skill in WEB_RESEARCHER_SKILLS {
        assert!(available.contains(skill), "Skill '{skill}' fehlt im Bundle");
        assert!(
            system.contains(&format!("`{skill}`")),
            "system.md nennt '{skill}' nicht"
        );
    }
    for needle in [
        "## Was ich NICHT tue",
        "## Evidenzpaket",
        "| URL | Abgerufen | Einstufung |",
        "Fakt, Behauptung oder Annahme",
        "Gegenquellen",
        "Für die Lage",
        "matrix.add_fact",
        "Privatpersonen",
        "Anmeldungen",
        "Bezahlschranken",
        "erfundenen",
    ] {
        assert!(system.contains(needle), "system.md: fehlt {needle}");
    }
    Ok(())
}

/// Plan R9: der Quellen-Skill beschreibt Suchstrategie, Primärquellen,
/// Querprüfung, Abrufdatum, die Einstufung in eigenen Worten und die
/// Sicherheitsgrenzen.
#[test]
fn test_osint_skill_covers_strategy_rating_and_safety() -> TestResult {
    let manifest: toml::Table = toml::from_str(bundled("skills/osint-web-research/skill.toml")?)
        .map_err(|error| format!("skill.toml: {error}"))?;
    let tools: Vec<&str> = manifest
        .get("tools")
        .and_then(toml::Value::as_array)
        .map(|list| list.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(tools, ["web.search", "web.fetch"]);
    let text = bundled("skills/osint-web-research/instructions.md")?;
    for needle in [
        "Suchanfragen variieren",
        "Primärquelle",
        "Querprüfen",
        "unabhängige",
        "Abrufdatum",
        "**Quelle (A–F)**",
        "**Information (1–6)**",
        "## Sicherheit und Grenzen",
        "Keine Personendaten über Privatpersonen",
        "Nichts hinter Anmeldungen",
        "Keine Paywall-Umgehung",
        "Robots und Nutzungsbedingungen",
        "Keine erfundenen Zahlen",
        "## Evidenzpaket",
    ] {
        assert!(text.contains(needle), "osint-web-research: fehlt {needle}");
    }
    Ok(())
}

/// Plan R9: der Szenario-Autor grundiert die Lage wie die Spielleitung mit
/// belegten Fakten (Workspace und Web-Recherche) statt pauschaler Annahmen.
#[test]
fn test_scenario_author_grounds_the_situation_with_sources() -> TestResult {
    let system = bundled("agents/matrix-scenario-author/system.md")?;
    for needle in [
        "## Lage mit Belegen",
        "SECURITY.md",
        "CHANGELOG.md",
        "`intel-web-researcher`",
        "(Quelle: ",
        "„Annahme:“",
    ] {
        assert!(
            system.contains(needle),
            "matrix-scenario-author: fehlt {needle}"
        );
    }
    Ok(())
}

#[test]
fn test_contains_word_respects_word_boundaries() {
    assert!(contains_word("laut nrc gilt", "nrc"));
    assert!(contains_word("(nrc)", "nrc"));
    assert!(!contains_word("unrcx", "nrc"));
    assert!(contains_word("miller/page schreiben", "miller/page"));
    assert!(!contains_word("heuertag", "heuer"));
}
