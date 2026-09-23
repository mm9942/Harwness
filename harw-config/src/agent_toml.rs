use serde::{Deserialize, Serialize};

/// Deklarative Agent-Definition aus `agent.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentToml {
    pub name: String,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_file: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Optional, advisory capabilities to make available in a child-agent
    /// spawn context. Suggestions are not activation grants.
    #[serde(default)]
    pub suggestions: AgentSuggestionsToml,
    #[serde(default)]
    pub primary_provider: Option<String>,
    #[serde(default)]
    pub secondary_providers: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSuggestionsToml {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub mcps: Vec<String>,
}

fn default_role() -> String {
    "worker".to_owned()
}
fn default_timeout() -> u64 {
    120
}
fn default_retries() -> u32 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn suggestion_lists_are_optional_and_default_empty() -> TestResult {
        let agent: AgentToml =
            toml::from_str("name = \"planner\"\n").map_err(ctx("minimales agent.toml parsen"))?;
        assert!(agent.suggestions.skills.is_empty());
        assert!(agent.suggestions.plugins.is_empty());

        let agent: AgentToml = toml::from_str(
            "name = \"planner\"\n[suggestions]\nskills = [\"research\"]\nmcps = [\"docs\"]\n",
        )
        .map_err(ctx("agent.toml mit suggestions parsen"))?;
        assert_eq!(agent.suggestions.skills, ["research"]);
        assert_eq!(agent.suggestions.mcps, ["docs"]);
        Ok(())
    }

    #[test]
    fn misspelled_authority_and_capability_fields_are_rejected() {
        for field in ["authoritiy", "capabilites"] {
            let input = format!("name = \"planner\"\n{field} = [\"missing\"]\n");
            assert!(toml::from_str::<AgentToml>(&input).is_err(), "{field}");
        }

        let input = "name = \"planner\"\n[suggestions]\nskils = [\"missing\"]\n";
        assert!(toml::from_str::<AgentToml>(input).is_err());
    }
}
