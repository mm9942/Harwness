use serde::{Deserialize, Serialize};

/// Declarative capability bundle manifest. Discovery reads manifests but never
/// loads executable plugin code; activation belongs to the runtime/policy
/// boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToml {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub source: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub capabilities: PluginCapabilitiesToml,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCapabilitiesToml {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcps: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub network_egress: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_manifest_defaults_to_enabled() {
        let plugin: PluginToml = toml::from_str(
            "name = \"review\"\nversion = \"1.0.0\"\n[capabilities]\nskills = [\"code-review\"]\n",
        )
        .unwrap();

        assert!(plugin.enabled);
        assert_eq!(plugin.capabilities.skills, ["code-review"]);
    }

    #[test]
    fn misspelled_authority_capability_and_reference_fields_are_rejected() {
        for field in ["authoritiy", "capabilites", "refernece"] {
            let input =
                format!("name = \"review\"\nversion = \"1.0.0\"\n{field} = [\"missing\"]\n");
            assert!(toml::from_str::<PluginToml>(&input).is_err(), "{field}");
        }

        for field in ["toools", "skils", "mcpss", "chanels", "network_egres"] {
            let input = format!(
                "name = \"review\"\nversion = \"1.0.0\"\n[capabilities]\n{field} = [\"missing\"]\n"
            );
            assert!(toml::from_str::<PluginToml>(&input).is_err(), "{field}");
        }
    }
}
