//! Secret-reference grammar und `auth.toml` (KEK-Provenance + Credential-Refs).
//!
//! `SecretRef` ist die einzige Art, wie ein Secret (Provider-API-Key,
//! Channel-Bot-Token, Webhook-Secret) in einer TOML-Datei erscheinen darf.
//! Ein literaler Klartextwert ist niemals ein gültiger `SecretRef` — das
//! Parsen schlägt hart fehl, statt den String stillschweigend als Secret zu
//! akzeptieren. Siehe `docs/design/config-structure.md` §4 und
//! `docs/design/secrets-and-audit.md`.
//!
//! `harw-config` löst `SecretRef`-Werte nicht auf (kein Zugriff auf
//! Umgebungsvariablen, Dateien, Keyring oder `harw-secrets` an dieser
//! Stelle) — das ist Aufgabe der aufrufenden Crate (`harw-provider`,
//! `harw-channel-telegram`, ...), sobald `harw-secrets` existiert.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

use crate::error::ConfigError;

/// Eine Zeigerreferenz auf ein Secret, niemals der Klartextwert selbst.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRef {
    /// `env:NAME` — aus einer Umgebungsvariable gelesen.
    Env(String),
    /// `file:PATH` — aus einer Datei gelesen (getrimmt).
    File(String),
    /// `keyring:ENTRY` — aus dem OS-Keyring gelesen.
    Keyring(String),
    /// `secrets:ID` — aus dem `harw-secrets`-SecretStore gelesen.
    Secrets(String),
    /// `file-json:PATH#/json/pointer` — ein Wert aus einer JSON-Datei, adressiert
    /// per JSON-Pointer. Der Pointer beginnt mit `/`. Split am *ersten* `#`.
    FileJson {
        /// Dateipfad vor dem ersten `#`.
        path: String,
        /// JSON-Pointer nach dem ersten `#` (beginnt mit `/`).
        pointer: String,
    },
}

impl SecretRef {
    /// Gibt die kanonische String-Form zurück (z. B. für Logging der
    /// *Referenz*, niemals des aufgelösten Werts).
    #[must_use]
    pub fn as_ref_string(&self) -> String {
        match self {
            Self::Env(v) => format!("env:{v}"),
            Self::File(v) => format!("file:{v}"),
            Self::Keyring(v) => format!("keyring:{v}"),
            Self::Secrets(v) => format!("secrets:{v}"),
            Self::FileJson { path, pointer } => format!("file-json:{path}#{pointer}"),
        }
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_ref_string())
    }
}

impl FromStr for SecretRef {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((prefix, rest)) = s.split_once(':') else {
            return Err(ConfigError::InvalidSecretRef(s.to_owned()));
        };
        if rest.is_empty() {
            return Err(ConfigError::InvalidSecretRef(s.to_owned()));
        }
        match prefix {
            "env" => Ok(Self::Env(rest.to_owned())),
            "file" => Ok(Self::File(rest.to_owned())),
            "keyring" => Ok(Self::Keyring(rest.to_owned())),
            "secrets" => Ok(Self::Secrets(rest.to_owned())),
            "file-json" => {
                let Some((path, pointer)) = rest.split_once('#') else {
                    return Err(ConfigError::InvalidSecretRef(s.to_owned()));
                };
                if path.is_empty() || !pointer.starts_with('/') {
                    return Err(ConfigError::InvalidSecretRef(s.to_owned()));
                }
                Ok(Self::FileJson {
                    path: path.to_owned(),
                    pointer: pointer.to_owned(),
                })
            }
            _ => Err(ConfigError::InvalidSecretRef(s.to_owned())),
        }
    }
}

impl Serialize for SecretRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_ref_string())
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// KEK-Provenance-Auswahl für `[kek]` in `auth.toml`.
/// Siehe `docs/design/secrets-and-audit.md` §3.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum KekProvenance {
    KeyFile,
    Keyring,
    EnvSeed,
}

/// `[kek]`-Tabelle in `auth.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KekConfig {
    pub provenance: KekProvenance,
    #[serde(default)]
    pub key_file_path: Option<String>,
    #[serde(default)]
    pub keyring_entry: Option<String>,
    #[serde(default)]
    pub env_seed_var: Option<String>,
}

/// Ein einzelner Credential-Eintrag im `credential_pool` — ein `SecretRef`
/// plus optionale Selektions-Metadaten (Label, Priorität, Basis-URL).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialEntry {
    /// Zeigerreferenz auf das eigentliche Secret.
    pub secret: SecretRef,
    /// Menschenlesbares Label zur Unterscheidung von Einträgen.
    #[serde(default)]
    pub label: Option<String>,
    /// Auswahlpriorität für die Runtime-Auswahl unter den Pool-Einträgen:
    /// **aufsteigend** sortiert probiert (`0` zuerst = am meisten
    /// bevorzugt), bei Gleichstand gewinnt die Reihenfolge in der Datei
    /// (siehe [`AuthConfig::credential_pool_ordered`]). Standard `0` — neue,
    /// per Onboarding angehängte Einträge landen damit automatisch am Ende
    /// gleich-priorisierter Einträge, ohne bestehende Priorisierungen zu
    /// verdrängen.
    ///
    /// Wichtig: `priority` ordnet nur die Pool-Einträge **untereinander**.
    /// Das primäre Credential — `provider.auth` (die einzelne `SecretRef`
    /// in `providers/<name>.toml`), bei Anthropic ersatzweise dessen
    /// implizite Umgebungs-Auflösung — wird von `harw-provider-http`
    /// immer zuerst versucht, unabhängig von jeder hier gesetzten
    /// `priority`; der Pool liefert ausschließlich Failover-Kandidaten
    /// dahinter (siehe `harw_provider_http::credential_pool`-Moduldoku).
    /// Nur wenn `provider.auth` fehlt (und bei Anthropic auch die implizite
    /// Auflösung keinen Treffer liefert), wird der nach `priority`
    /// niedrigste Pool-Eintrag selbst zum primären Credential.
    #[serde(default)]
    pub priority: u32,
    /// Optionale Basis-URL, die diesem Credential zugeordnet ist.
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Vollständige `auth.toml`: KEK-Provenance + benannte Credential-Refs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    #[serde(default)]
    pub kek: Option<KekConfig>,
    /// Frei benannte Credential-Refs, z. B. `openai-default = "env:OPENAI_API_KEY"`.
    #[serde(default)]
    pub credentials: std::collections::HashMap<String, SecretRef>,
    /// Benannte Pools mehrerer `CredentialEntry`, z. B. für rotierende oder
    /// priorisierte Provider-Credentials.
    #[serde(default)]
    pub credential_pool: std::collections::HashMap<String, Vec<CredentialEntry>>,
}

impl AuthConfig {
    /// Liefert die `credential_pool`-Einträge eines Providers in
    /// Auswahlreihenfolge: **aufsteigend** nach [`CredentialEntry::priority`]
    /// sortiert (`0` zuerst), bei Gleichstand stabil in der Reihenfolge, in
    /// der die Einträge in der TOML-Datei stehen (`Vec::sort_by_key` ist
    /// stabil).
    ///
    /// Aufrufer (z. B. `harw-provider-http`) nutzen dies, um bei mehreren
    /// Credentials für denselben Provider (z. B. mehrere `openai`- oder
    /// `cloudflare`-Schlüssel) deterministisch zu wählen, welcher zuerst
    /// versucht wird, und welcher als Nächstes an der Reihe ist, wenn der
    /// aktuell aktive Eintrag ausfällt (Failover).
    ///
    /// # Arguments
    /// - `provider` (`&str`): Providername, wie er auch als TOML-Array-Name
    ///   in `[[credential_pool.<provider>]]` erscheint.
    ///
    /// # Returns
    /// Eine leere `Vec`, wenn `provider` keinen Pool besitzt; sonst die
    /// Referenzen in Auswahlreihenfolge.
    ///
    /// # Examples
    /// ```rust
    /// use harw_config::AuthConfig;
    ///
    /// let toml_src = r#"
    ///     [[credential_pool.openai]]
    ///     secret = "env:OPENAI_KEY_B"
    ///     priority = 5
    ///
    ///     [[credential_pool.openai]]
    ///     secret = "env:OPENAI_KEY_A"
    /// "#;
    /// let cfg: AuthConfig = toml::from_str(toml_src).expect("valid auth.toml");
    /// let ordered = cfg.credential_pool_ordered("openai");
    /// assert_eq!(ordered[0].secret.as_ref_string(), "env:OPENAI_KEY_A");
    /// assert_eq!(ordered[1].secret.as_ref_string(), "env:OPENAI_KEY_B");
    /// ```
    #[must_use]
    pub fn credential_pool_ordered(&self, provider: &str) -> Vec<&CredentialEntry> {
        let mut entries: Vec<&CredentialEntry> = self
            .credential_pool
            .get(provider)
            .map(|pool| pool.iter().collect())
            .unwrap_or_default();
        entries.sort_by_key(|entry| entry.priority);
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_secretref_fromstr_env_ok() -> TestResult {
        let r: SecretRef = "env:OPENAI_API_KEY".parse().map_err(ctx("parse env ref"))?;
        assert_eq!(r, SecretRef::Env("OPENAI_API_KEY".to_owned()));
        Ok(())
    }

    #[test]
    fn test_secretref_fromstr_rejects_bare_value() {
        let r: Result<SecretRef, _> = "sk-not-a-ref-value".parse();
        assert!(matches!(r, Err(ConfigError::InvalidSecretRef(_))));
    }

    #[test]
    fn test_secretref_fromstr_rejects_unknown_prefix() {
        let r: Result<SecretRef, _> = "vault:foo".parse();
        assert!(matches!(r, Err(ConfigError::InvalidSecretRef(_))));
    }

    #[test]
    fn test_auth_config_parses_toml() -> TestResult {
        let toml_src = r#"
            [kek]
            provenance = "key_file"
            key_file_path = "~/.config/harwness/kek.seed"

            [credentials]
            openai-default = "env:OPENAI_API_KEY"
        "#;
        let cfg: AuthConfig = toml::from_str(toml_src).map_err(ctx("parse toml"))?;
        assert_eq!(
            cfg.credentials.get("openai-default"),
            Some(&SecretRef::Env("OPENAI_API_KEY".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn test_secretref_filejson_roundtrip() -> TestResult {
        let raw = "file-json:/etc/creds.json#/providers/openai/key";
        let parsed: SecretRef = raw.parse().map_err(ctx("parse file-json ref"))?;
        assert_eq!(
            parsed,
            SecretRef::FileJson {
                path: "/etc/creds.json".to_owned(),
                pointer: "/providers/openai/key".to_owned(),
            }
        );
        assert_eq!(parsed.as_ref_string(), raw);
        assert_eq!(parsed.to_string(), raw);
        Ok(())
    }

    #[test]
    fn test_secretref_filejson_rejects_pointer_without_slash() {
        let r: Result<SecretRef, _> = "file-json:/etc/creds.json#nopointer".parse();
        assert!(matches!(r, Err(ConfigError::InvalidSecretRef(_))));
    }

    #[test]
    fn test_secretref_filejson_rejects_missing_hash() {
        let r: Result<SecretRef, _> = "file-json:/etc/creds.json".parse();
        assert!(matches!(r, Err(ConfigError::InvalidSecretRef(_))));
    }

    #[test]
    fn test_secretref_filejson_splits_at_first_hash() -> TestResult {
        let parsed: SecretRef = "file-json:/a/b.json#/x#y"
            .parse()
            .map_err(ctx("parse file-json ref"))?;
        assert_eq!(
            parsed,
            SecretRef::FileJson {
                path: "/a/b.json".to_owned(),
                pointer: "/x#y".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn test_auth_config_parses_credential_pool() -> TestResult {
        let toml_src = r#"
            [[credential_pool.openai]]
            secret = "env:OPENAI_API_KEY"
            label = "primary"
            priority = 10
            base_url = "https://api.openai.com/v1"

            [[credential_pool.openai]]
            secret = "file-json:/etc/creds.json#/openai/key"
        "#;
        let cfg: AuthConfig = toml::from_str(toml_src).map_err(ctx("parse toml"))?;
        let pool = cfg
            .credential_pool
            .get("openai")
            .ok_or(TestError::Missing("credential_pool.openai"))?;
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0].secret, SecretRef::Env("OPENAI_API_KEY".to_owned()));
        assert_eq!(pool[0].label.as_deref(), Some("primary"));
        assert_eq!(pool[0].priority, 10);
        assert_eq!(
            pool[0].base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            pool[1].secret,
            SecretRef::FileJson {
                path: "/etc/creds.json".to_owned(),
                pointer: "/openai/key".to_owned(),
            }
        );
        assert_eq!(pool[1].priority, 0);
        assert!(pool[1].label.is_none());
        Ok(())
    }

    #[test]
    fn test_auth_config_rejects_misspelled_credential_entry_field() {
        let toml_src = r#"
            [[credential_pool.openai]]
            secret = "env:OPENAI_API_KEY"
            lable = "primary"
        "#;

        assert!(toml::from_str::<AuthConfig>(toml_src).is_err());
    }

    #[test]
    fn test_auth_config_rejects_misspelled_credential_pool_field() {
        let toml_src = r#"
            [credential_pools.openai]
            secret = "env:OPENAI_API_KEY"
        "#;

        assert!(toml::from_str::<AuthConfig>(toml_src).is_err());
    }

    #[test]
    fn test_credential_pool_ordered_sorts_ascending_by_priority() -> TestResult {
        let toml_src = r#"
            [[credential_pool.openai]]
            secret = "env:OPENAI_KEY_HIGH_PRIORITY_NUMBER"
            priority = 10

            [[credential_pool.openai]]
            secret = "env:OPENAI_KEY_DEFAULT_A"

            [[credential_pool.openai]]
            secret = "env:OPENAI_KEY_DEFAULT_B"
        "#;
        let cfg: AuthConfig = toml::from_str(toml_src).map_err(ctx("parse toml"))?;
        let ordered = cfg.credential_pool_ordered("openai");
        // Aufsteigend nach priority (0 vor 10); bei Gleichstand (0 == 0)
        // gewinnt die Datei-Reihenfolge (A vor B) — stabile Sortierung.
        assert_eq!(
            ordered
                .iter()
                .map(|entry| entry.secret.as_ref_string())
                .collect::<Vec<_>>(),
            vec![
                "env:OPENAI_KEY_DEFAULT_A".to_owned(),
                "env:OPENAI_KEY_DEFAULT_B".to_owned(),
                "env:OPENAI_KEY_HIGH_PRIORITY_NUMBER".to_owned(),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_credential_pool_ordered_empty_for_unknown_provider() {
        let cfg = AuthConfig::default();
        assert!(cfg.credential_pool_ordered("openai").is_empty());
    }
}
