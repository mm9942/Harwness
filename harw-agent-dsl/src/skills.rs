//! Skill-Bindung einer Agentendefinition (`skills = [...]`).
//!
//! Dieses Modul bündelt die Regeln für die erstklassige Liste
//! [`RawAgentDefinition::skills`](crate::raw::RawAgentDefinition::skills):
//! Namensvalidierung, Vereinigung über `extends`/Mixins/Schichten und die
//! Ablage in der aufgelösten Definition.
//!
//! # Namensregeln
//! Ein Skill-Name besteht nur aus `[a-z0-9-]`, ist nicht leer und höchstens
//! [`MAX_SKILL_NAME_LEN`] Zeichen lang. Innerhalb **einer** Definition darf
//! derselbe Name nicht doppelt vorkommen (Tippfehler-Schutz).
//!
//! # Vererbung
//! Skills sind **additiv** (Vereinigung mit Duplikat-Entfernung, erste
//! Nennung gewinnt die Position): Basis (`extends`, älteste Vorfahren zuerst)
//! → Mixins in Deklarationsreihenfolge → eigene Liste → höhere Schichten.
//! Eine Kind-Definition kann geerbte Skills nicht stillschweigend verlieren;
//! entfernen oder ersetzen lässt sich die geerbte Liste nur explizit über
//! `[patch.skills]` (`remove`/`replace`/`intersect`/`append`/`prepend`, §7).
//! Skills sind reine Instruktionsfragmente und tragen keine Authority — die
//! Vereinigung kann deshalb keine Rechte ausweiten.
//!
//! # Ablage
//! [`ResolvedAgentDefinition`](crate::resolved::ResolvedAgentDefinition) führt
//! die aufgelöste Liste unter dem reservierten Konfigurationsschlüssel
//! [`SKILLS_CONFIG_KEY`] (nur, wenn sie nicht leer ist); das hält die
//! öffentlichen Struktur-Literale dieses Typs quellkompatibel.
//! [`crate::lower`] liest sie von dort streng (fail-closed) in
//! [`ExecutableAgentIr::skills`](crate::ExecutableAgentIr::skills).

use crate::error::{DiagLocation, DslError, DslResult};
use crate::ids::DefinitionId;

/// Höchstlänge eines Skill-Namens in Zeichen.
pub const MAX_SKILL_NAME_LEN: usize = 64;

/// Reservierter Schlüssel in `ResolvedAgentDefinition::config`, unter dem die
/// aufgelöste Skill-Liste als String-Array liegt.
pub const SKILLS_CONFIG_KEY: &str = "skills";

/// Prüft einen einzelnen Skill-Namen.
///
/// # Rückgabe
/// `Ok(())` für einen gültigen Namen, sonst `Err` mit einer deutschen
/// Begründung.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::skills::validate_skill_name;
///
/// assert!(validate_skill_name("code-review").is_ok());
/// assert!(validate_skill_name("Code_Review").is_err());
/// ```
pub fn validate_skill_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Skill-Name darf nicht leer sein");
    }
    if name.len() > MAX_SKILL_NAME_LEN {
        return Err("Skill-Name ist länger als 64 Zeichen");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("Skill-Name darf nur a-z, 0-9 und '-' enthalten");
    }
    Ok(())
}

/// Prüft die Skill-Liste einer einzelnen Definition: jeder Name gültig, kein
/// Name doppelt.
///
/// # Fehler
/// [`DslError::InvalidSkill`] mit Feldpfad `skills[i]` (bzw. `field_prefix[i]`).
pub(crate) fn validate_skill_list(
    of: &DefinitionId,
    skills: &[String],
    field_prefix: &str,
) -> DslResult<()> {
    for (index, name) in skills.iter().enumerate() {
        let invalid = |reason: &'static str| DslError::InvalidSkill {
            of: Box::new(of.clone()),
            skill: name.clone(),
            reason,
            location: DiagLocation::field(format!("{field_prefix}[{index}]")),
        };
        if let Err(reason) = validate_skill_name(name) {
            return Err(invalid(reason));
        }
        if skills[..index].contains(name) {
            return Err(invalid("Skill ist in derselben Liste doppelt aufgeführt"));
        }
    }
    Ok(())
}

/// Vereinigt `additional` in `accumulated` (Reihenfolge erhalten, Duplikate
/// entfernt; die erste Nennung behält ihre Position).
pub(crate) fn union_skills(accumulated: &mut Vec<String>, additional: &[String]) {
    for name in additional {
        if !accumulated.contains(name) {
            accumulated.push(name.clone());
        }
    }
}

/// Liest die Skill-Liste nachsichtig aus einer aufgelösten Konfiguration
/// (Nicht-Strings und ein Nicht-Array ergeben eine leere bzw. gekürzte Liste).
pub(crate) fn skills_from_config_lenient(config: &toml::Table) -> Vec<String> {
    config
        .get(SKILLS_CONFIG_KEY)
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Liest die Skill-Liste streng aus einer aufgelösten Konfiguration.
///
/// # Fehler
/// [`DslError::InvalidSkill`], wenn der Schlüssel kein Array aus Strings ist,
/// ein Name ungültig ist oder doppelt vorkommt — die IR erfindet aus
/// fehlerhafter Eingabe keine Skills und verschluckt auch keine.
pub(crate) fn skills_from_config_strict(
    of: &DefinitionId,
    config: &toml::Table,
) -> DslResult<Vec<String>> {
    let Some(value) = config.get(SKILLS_CONFIG_KEY) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(DslError::InvalidSkill {
            of: Box::new(of.clone()),
            skill: value.to_string(),
            reason: "'skills' muss eine Liste von Strings sein",
            location: DiagLocation::field(SKILLS_CONFIG_KEY),
        });
    };
    let mut skills = Vec::with_capacity(values.len());
    for (index, entry) in values.iter().enumerate() {
        let Some(name) = entry.as_str() else {
            return Err(DslError::InvalidSkill {
                of: Box::new(of.clone()),
                skill: entry.to_string(),
                reason: "Skill-Eintrag ist kein String",
                location: DiagLocation::field(format!("{SKILLS_CONFIG_KEY}[{index}]")),
            });
        };
        skills.push(name.to_owned());
    }
    validate_skill_list(of, &skills, SKILLS_CONFIG_KEY)?;
    Ok(skills)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn id() -> TestResult<DefinitionId> {
        Ok(DefinitionId::parse("harwness.agent.skill-test@1")?)
    }

    #[test]
    fn test_valid_names() {
        let longest = "x".repeat(64);
        for name in ["a", "code-review", "rust2024", longest.as_str()] {
            assert!(validate_skill_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn test_invalid_names() {
        let too_long = "x".repeat(65);
        for name in ["", "Code", "a_b", "a b", "ä", "a.b", too_long.as_str()] {
            assert!(validate_skill_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn test_duplicate_in_one_list_rejected_with_index() -> TestResult {
        let skills = vec!["a".to_owned(), "b".to_owned(), "a".to_owned()];
        let Err(error) = validate_skill_list(&id()?, &skills, "skills") else {
            return Err(crate::test_support::TestError::Unexpected(
                "Duplikat muss abgelehnt werden".into(),
            ));
        };
        let message = error.to_string();
        assert!(message.contains("skills[2]"), "{message}");
        Ok(())
    }

    #[test]
    fn test_union_dedupes_and_keeps_first_position() {
        let mut acc = vec!["a".to_owned(), "b".to_owned()];
        union_skills(&mut acc, &["c".to_owned(), "a".to_owned(), "d".to_owned()]);
        assert_eq!(acc, ["a", "b", "c", "d"]);
    }

    #[test]
    fn test_strict_config_read_rejects_non_strings() -> TestResult {
        let mut config = toml::Table::new();
        config.insert(
            SKILLS_CONFIG_KEY.to_owned(),
            toml::Value::Array(vec![toml::Value::Integer(1)]),
        );
        assert!(skills_from_config_strict(&id()?, &config).is_err());
        assert!(skills_from_config_lenient(&config).is_empty());
        config.insert(
            SKILLS_CONFIG_KEY.to_owned(),
            toml::Value::String("a".to_owned()),
        );
        assert!(skills_from_config_strict(&id()?, &config).is_err());
        Ok(())
    }

    #[test]
    fn test_strict_config_read_absent_is_empty() -> TestResult {
        assert!(skills_from_config_strict(&id()?, &toml::Table::new())?.is_empty());
        Ok(())
    }
}
