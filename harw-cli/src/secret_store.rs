//! CLI composition for configured `secrets:` provider credentials.
//!
//! This module is deliberately narrow: it only opens the sealed store when an
//! enabled provider actually needs it, and it never falls back to the legacy
//! plaintext `<home>/secrets` directory.

use std::path::{Path, PathBuf};

use harw_config::{KekConfig, KekProvenance as ConfigKekProvenance, ResolvedConfig, SecretRef};
use harw_provider_http::SecretResolver;
use harw_secrets::audit::chain::PersistedChainStatus;
use harw_secrets::{
    AuditResult, CryptoPolicy, KekProvenance, KeyVersion, SecretStore, SecretsError,
    load_kek_material,
};
use secrecy::SecretString;
use secrecy_08::ExposeSecret as _;

/// A synchronous provider-secret resolver backed by the sealed secret store.
pub struct ConfiguredSecretResolver {
    store: SecretStore,
}

impl ConfiguredSecretResolver {
    /// Creates a resolver for an already-opened sealed secret store.
    #[must_use]
    pub fn new(store: SecretStore) -> Self {
        Self { store }
    }
}

impl SecretResolver for ConfiguredSecretResolver {
    fn resolve(&self, reference: &str) -> Result<SecretString, String> {
        let secret = self
            .store
            .get_by_reference(reference)
            .map_err(resolve_error_message)?;
        let value = String::from_utf8(secret.expose_secret().as_ref().to_vec())
            .map_err(|_| "sealed secret is not valid UTF-8".to_owned())?;
        Ok(SecretString::from(value))
    }
}

/// Maps a store error to an operator-facing message without secret content.
///
/// A record sealed under a retired pure ML-KEM level is reported with a
/// migration hint (only the algorithm name is shown); every other error stays
/// masked.
fn resolve_error_message(error: SecretsError) -> String {
    match error {
        SecretsError::UnsupportedLegacyKem { algo } => format!(
            "sealed secret uses the retired KEM '{algo}' and can no longer be opened; \
             re-create the secret under a hybrid KEM (see docs/setup/crypt-guard.md)"
        ),
        _ => "sealed secret could not be resolved".to_owned(),
    }
}

/// Opens a sealed provider-secret resolver when an enabled provider needs one.
///
/// The sealed store is rooted at `<home>/sealed-secrets`, intentionally
/// distinct from the legacy plaintext `<home>/secrets` directory. KEK material
/// is loaded only after configuration proves that at least one enabled provider
/// has an `auth = "secrets:…"` reference.
pub fn open_configured_secret_resolver(
    home: &Path,
    config: &ResolvedConfig,
) -> Result<Option<ConfiguredSecretResolver>, String> {
    if !configured_provider_uses_sealed_secret(config) {
        return Ok(None);
    }

    let kek = config
        .auth
        .kek
        .as_ref()
        .ok_or_else(|| "enabled sealed-secret provider requires a configured KEK".to_owned())?;
    let provenance = configured_kek_provenance(kek)?;
    let policy = CryptoPolicy::strongest();
    let key_material = load_kek_material(&policy, &provenance)
        .map_err(|_| "sealed secret resolver could not load KEK material".to_owned())?;
    let store = SecretStore::open_with_key_material(
        home.join("sealed-secrets"),
        policy,
        provenance,
        KeyVersion::initial(),
        key_material,
    )
    .map_err(|_| "sealed secret resolver could not open sealed secret store".to_owned())?;

    Ok(Some(ConfiguredSecretResolver::new(store)))
}

/// Prüft die **persistierte** Audit-Kette (`audit.log` auf der Platte) des
/// konfigurierten Geheimnisspeichers, für die periodische Kettenprüfung in
/// `crate::gateway::audit_chain_scheduler`.
///
/// # Description
/// Öffnet den versiegelten Speicher **exakt** über [`open_configured_secret_resolver`]
/// — dasselbe Gate (nur wenn ein aktivierter Provider tatsächlich eine
/// `secrets:`-Referenz nutzt und ein KEK konfiguriert ist), dieselbe
/// KEK-Auflösung, derselbe Pfad — und ruft anschließend
/// [`SecretStore::verify_persisted_audit_chain`] auf. Das ist ein bewusster
/// Wechsel gegenüber der vorherigen Fassung dieser Funktion (die
/// [`SecretStore::audit_log`] zurückgab): jene Kette ist die **im Speicher
/// gehaltene** Kette einer frisch geöffneten Instanz, die
/// [`SecretStore::open`]/[`SecretStore::open_with_key_material`] stets leer
/// beginnen und die nur um Mutationen wächst, die innerhalb genau dieser
/// Instanz passieren — der Gateway-Prozess ruft nie `create`/`rotate`/
/// `delete` auf, also war diese Kette in der Praxis immer leer und eine
/// Manipulationsprüfung darauf prüfte nichts. Die auf der Platte
/// persistierte Datei ist dagegen von genau der Angriffsrichtung betroffen,
/// die diese periodische Prüfung entdecken soll.
///
/// **Warum je Aufruf neu öffnen, statt ein Handle zu halten:** unverändert
/// gegenüber der vorherigen Fassung. `open_configured_secret_resolver` hält
/// den Speicher schon heute nur für die Dauer eines einzigen Aufrufs offen
/// — das begrenzt, wie lange entschlüsseltes KEK-Material im Prozessspeicher
/// lebt. Diese Funktion respektiert dasselbe Prinzip: ein Scheduler, der
/// stattdessen ein Handle über viele Ticks hinweg hielte, würde die
/// Lebensdauer des entschlüsselten Schlüsselmaterials von „Sekunden bei
/// jedem Tick" auf „die gesamte Gateway-Laufzeit" verlängern, ohne einen
/// Gegenwert an geprüfter Kettentiefe zu gewinnen: [`SecretStore::verify_persisted_audit_chain`]
/// liest die Kette ohnehin frisch von der Platte, unabhängig davon, wie
/// lange die Instanz schon offen ist.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space, siehe [`open_configured_secret_resolver`].
/// - `config` (`&ResolvedConfig`): aufgelöste Konfiguration, siehe
///   [`open_configured_secret_resolver`].
///
/// # Returns
/// `Ok(None)`, wenn kein aktivierter Provider eine `secrets:`-Referenz
/// nutzt — dann ist nichts zu prüfen, und der Aufrufer darf das nicht als
/// „geprüft, unversehrt" ausgeben. `Ok(Some(status))` sonst, wobei `status`
/// selbst [`AuditResult<PersistedChainStatus>`] ist und die drei Fälle aus
/// [`harw_secrets::audit::chain::load_and_verify_persisted_chain`]
/// unterscheidet: [`PersistedChainStatus::Absent`] (noch nichts
/// protokolliert — kein Fund), [`PersistedChainStatus::Intact`] (Datei
/// gelesen und verkettet unversehrt), oder ein `Err` — [`harw_secrets::AuditError::Io`]
/// für eine unlesbare/nicht dekodierbare Datei (kein Fund, kein Bruch —
/// „wir konnten nicht nachsehen"), [`harw_secrets::AuditError::ChainBroken`]
/// für einen tatsächlich festgestellten Kettenbruch.
///
/// # Errors
/// Ein `String` ohne Geheimnisinhalt, wenn KEK-Konfiguration, KEK-Material
/// oder das Öffnen des Speichers selbst fehlschlägt — identisch zu
/// [`open_configured_secret_resolver`]s Fehlerfällen. Das ist eine andere
/// Fehlerebene als das innere `AuditResult`: dieser äußere `Err` bedeutet
/// „der Speicher konnte gar nicht erst geöffnet werden", nicht „die Kette
/// ist gebrochen".
///
/// # Concurrency
/// Rein synchron (Datei-/Schlüssel-I/O); Aufrufer in `crate::gateway` führen
/// dies über `tokio::task::spawn_blocking` aus, nie direkt aus der
/// Event-Loop heraus.
pub fn configured_secret_store_persisted_audit_chain_status(
    home: &Path,
    config: &ResolvedConfig,
) -> Result<Option<AuditResult<PersistedChainStatus>>, String> {
    let resolver = open_configured_secret_resolver(home, config)?;
    Ok(resolver.map(|resolver| resolver.store.verify_persisted_audit_chain()))
}

fn configured_provider_uses_sealed_secret(config: &ResolvedConfig) -> bool {
    config
        .providers
        .values()
        .any(|provider| provider.enabled && matches!(&provider.auth, Some(SecretRef::Secrets(_))))
}

fn configured_kek_provenance(kek: &KekConfig) -> Result<KekProvenance, String> {
    match &kek.provenance {
        ConfigKekProvenance::KeyFile => {
            let path = required_config_value(kek.key_file_path.as_deref(), "key-file path")?;
            Ok(KekProvenance::KeyFile {
                path: expand_leading_home(path)?,
            })
        }
        ConfigKekProvenance::EnvSeed => {
            let var =
                required_config_value(kek.env_seed_var.as_deref(), "environment seed variable")?;
            Ok(KekProvenance::EnvSeed {
                var: var.to_owned(),
            })
        }
        ConfigKekProvenance::Keyring => {
            let entry = required_config_value(kek.keyring_entry.as_deref(), "keyring entry")?;
            let (service, account) = entry
                .split_once('/')
                .ok_or_else(|| "keyring KEK entry must be service/account".to_owned())?;
            if service.is_empty() || account.is_empty() || account.contains('/') {
                return Err("keyring KEK entry must be service/account".to_owned());
            }
            Ok(KekProvenance::OsKeyring {
                service: service.to_owned(),
                account: account.to_owned(),
            })
        }
    }
}

fn required_config_value<'a>(value: Option<&'a str>, label: &str) -> Result<&'a str, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("configured KEK requires a non-empty {label}"))
}

fn expand_leading_home(path: &str) -> Result<PathBuf, String> {
    let Some(remainder) = path.strip_prefix("~/") else {
        return Ok(PathBuf::from(path));
    };
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| "cannot expand key-file path because HOME is unavailable".to_owned())?;
    Ok(PathBuf::from(home).join(remainder))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use harw_config::{KekConfig, KekProvenance as ConfigKekProvenance, ProviderToml};
    use harw_secrets::audit::chain::PersistedChainStatus;
    use harw_secrets::{
        AuditError, CryptoPolicy, KekProvenance, KeyVersion, SecretStore, load_kek_material,
    };
    use secrecy::ExposeSecret as _;
    use secrecy_08::SecretBox;
    use tempfile::TempDir;

    use super::{
        ConfiguredSecretResolver, SecretResolver,
        configured_secret_store_persisted_audit_chain_status, open_configured_secret_resolver,
    };

    #[test]
    fn no_secrets_provider_reference_returns_none_without_a_kek() {
        let config = harw_config::ResolvedConfig::default();
        let home = TempDir::new().expect("temporary home");

        assert!(
            open_configured_secret_resolver(home.path(), &config)
                .expect("no sealed provider must not require a KEK")
                .is_none()
        );
    }

    #[test]
    fn sealed_provider_without_a_kek_fails_closed() {
        let config = sealed_provider_config();
        let home = TempDir::new().expect("temporary home");

        let error = match open_configured_secret_resolver(home.path(), &config) {
            Ok(_) => panic!("sealed provider without KEK must fail"),
            Err(error) => error,
        };
        assert!(error.contains("requires a configured KEK"));
    }

    #[test]
    fn invalid_keyring_kek_fails_closed_when_sealed_provider_is_configured() {
        let mut config = sealed_provider_config();
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::Keyring,
            key_file_path: None,
            keyring_entry: Some("missing-account".to_owned()),
            env_seed_var: None,
        });
        let home = TempDir::new().expect("temporary home");

        let error = match open_configured_secret_resolver(home.path(), &config) {
            Ok(_) => panic!("malformed keyring entry must fail"),
            Err(error) => error,
        };
        assert_eq!(error, "keyring KEK entry must be service/account");
    }

    #[test]
    fn resolver_returns_a_valid_utf8_sealed_secret() {
        let home = TempDir::new().expect("temporary home");
        let key_path = write_test_kek(home.path());
        let mut store = store_with_test_kek(home.path(), &key_path);
        store
            .create(
                "provider-token",
                "provider authentication",
                &SecretBox::new(b"utf8-token".to_vec().into_boxed_slice()),
            )
            .expect("seal test token");
        let resolver = ConfiguredSecretResolver::new(store);

        let resolved = resolver
            .resolve("provider-token")
            .expect("resolve valid UTF-8 secret");
        assert_eq!(resolved.expose_secret(), "utf8-token");
    }

    #[test]
    fn resolver_rejects_non_utf8_sealed_secret_without_leaking_data() {
        let home = TempDir::new().expect("temporary home");
        let key_path = write_test_kek(home.path());
        let mut store = store_with_test_kek(home.path(), &key_path);
        let secret_bytes = [0xff, 0x00, 0x80];
        store
            .create(
                "binary-provider-token",
                "provider authentication",
                &SecretBox::new(secret_bytes.to_vec().into_boxed_slice()),
            )
            .expect("seal binary test token");
        let resolver = ConfiguredSecretResolver::new(store);

        let error = resolver
            .resolve("binary-provider-token")
            .expect_err("non-UTF-8 secret must be rejected");
        assert_eq!(error, "sealed secret is not valid UTF-8");
        assert!(!error.contains("255"));
        assert!(!error.contains("binary-provider-token"));
    }

    #[test]
    fn resolver_reports_a_retired_kem_with_a_migration_hint() {
        let home = TempDir::new().expect("temporary home");
        let key_path = write_test_kek(home.path());
        {
            let mut store = store_with_test_kek(home.path(), &key_path);
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"legacy-token".to_vec().into_boxed_slice()),
                )
                .expect("seal test token");
        }
        // Reset the durable KEM name to a retired pure ML-KEM level, as written
        // before the hybrid switch.
        let entry = fs::read_dir(home.path().join("sealed-secrets").join("secrets"))
            .expect("sealed secrets directory")
            .next()
            .expect("one sealed entry")
            .expect("sealed entry")
            .path();
        let mut durable: serde_json::Value =
            serde_json::from_slice(&fs::read(&entry).expect("read sealed entry"))
                .expect("sealed entry is JSON");
        durable["record"]["kem_algo"] = serde_json::Value::String("ml_kem_768".to_owned());
        let legacy_entry = serde_json::to_vec(&durable).expect("encode legacy entry");
        fs::write(&entry, legacy_entry).expect("write legacy entry");

        let policy = CryptoPolicy::strongest();
        let provenance = KekProvenance::KeyFile { path: key_path };
        let key_material = load_kek_material(&policy, &provenance).expect("load test KEK material");
        let store = SecretStore::open_with_key_material(
            home.path().join("sealed-secrets"),
            policy,
            provenance,
            KeyVersion::initial(),
            key_material,
        )
        .expect("legacy record still loads");
        let resolver = ConfiguredSecretResolver::new(store);

        let error = resolver
            .resolve("secrets:provider-token")
            .expect_err("retired KEM must not resolve");
        assert!(error.contains("retired KEM 'ml_kem_768'"));
        assert!(error.contains("re-create the secret"));
        assert!(error.contains("docs/setup/crypt-guard.md"));
        assert!(!error.contains("legacy-token"));

        let masked = resolver
            .resolve("secrets:missing-provider-token")
            .expect_err("unknown reference must not resolve");
        assert_eq!(masked, "sealed secret could not be resolved");
        assert!(!masked.contains("missing-provider-token"));
    }

    /// Der wichtigste Test dieses Knotens: die periodische Kettenprüfung
    /// bezieht sich auf die Kette des **konfigurierten** `SecretStore` — nicht
    /// auf eine unabhängig davon fabrizierte Kette. Ohne konfigurierten
    /// Speicher gibt es nichts zu prüfen.
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_returns_none_without_a_sealed_provider()
    {
        let config = harw_config::ResolvedConfig::default();
        let home = TempDir::new().expect("temporary home");

        assert!(
            configured_secret_store_persisted_audit_chain_status(home.path(), &config)
                .expect("no sealed provider must not require a KEK")
                .is_none()
        );
    }

    #[test]
    fn configured_secret_store_persisted_audit_chain_status_fails_closed_without_a_kek() {
        let config = sealed_provider_config();
        let home = TempDir::new().expect("temporary home");

        let error = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .expect_err("sealed provider without KEK must fail closed");
        assert!(error.contains("requires a configured KEK"));
    }

    /// Der wichtigste Test dieses Knotens: die geprüfte Kette ist die auf der
    /// Platte persistierte `audit.log`-Datei — nicht die im Speicher
    /// gehaltene Kette der frisch geöffneten Instanz (die stets leer wäre,
    /// siehe `SecretStore::open_with_key_material`s Doku). Eine Mutation
    /// durch eine separate `SecretStore`-Instanz an derselben Wurzel muss
    /// hier sichtbar werden, weil sie auf die Platte durchgeschrieben wurde.
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_reads_the_real_disk_file() {
        let home = TempDir::new().expect("temporary home");
        let key_path = write_test_kek(home.path());
        {
            let mut store = store_with_test_kek(home.path(), &key_path);
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"utf8-token".to_vec().into_boxed_slice()),
                )
                .expect("seal test token");
        }
        let mut config = sealed_provider_config();
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });

        let status = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .expect("valid KEK must open the configured store")
            .expect("sealed provider is configured")
            .expect("a freshly written chain must verify intact");

        match status {
            PersistedChainStatus::Intact { event_count, .. } => {
                assert_eq!(
                    event_count, 1,
                    "the disk-persisted chain must show the mutation made by the earlier instance"
                );
            }
            other => panic!("expected an intact persisted chain, got {other:?}"),
        }
    }

    /// Ohne jede Mutation existiert `audit.log` noch nicht — das ist kein
    /// Fund, sondern „nichts protokolliert".
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_reports_absent_without_mutations() {
        let home = TempDir::new().expect("temporary home");
        let key_path = write_test_kek(home.path());
        let mut config = sealed_provider_config();
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });

        let status = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .expect("valid KEK must open the configured store")
            .expect("sealed provider is configured");

        assert!(matches!(status, Ok(PersistedChainStatus::Absent)));
    }

    #[test]
    fn configured_secret_store_persisted_audit_chain_status_never_leaks_secret_content_on_open_failure()
     {
        let mut config = sealed_provider_config();
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::Keyring,
            key_file_path: None,
            keyring_entry: Some("missing-account".to_owned()),
            env_seed_var: None,
        });
        let home = TempDir::new().expect("temporary home");

        let error = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .expect_err("malformed keyring entry must fail");
        assert_eq!(error, "keyring KEK entry must be service/account");
        assert!(!error.contains("provider-token"));
    }

    /// Ein Lesefehler der persistierten Kette (hier: eine abgeschnittene
    /// Datei) trägt weder Geheimnisinhalt noch Kettenereignisdaten in seiner
    /// `Display`-Meldung.
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_io_error_carries_no_secret_content() {
        let home = TempDir::new().expect("temporary home");
        let key_path = write_test_kek(home.path());
        {
            let mut store = store_with_test_kek(home.path(), &key_path);
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"utf8-token".to_vec().into_boxed_slice()),
                )
                .expect("seal test token");
        }
        let audit_log_path = home.path().join("sealed-secrets").join("audit.log");
        let bytes = std::fs::read(&audit_log_path).expect("read persisted audit log");
        std::fs::write(&audit_log_path, &bytes[..bytes.len() / 2])
            .expect("truncate persisted audit log");
        let mut config = sealed_provider_config();
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });

        let status = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .expect("valid KEK must open the configured store")
            .expect("sealed provider is configured");

        let error = match status {
            Err(error @ AuditError::Io(_)) => error,
            other => panic!("expected an unreadable persisted chain, got {other:?}"),
        };
        assert!(!error.to_string().contains("provider-token"));
        assert!(!error.to_string().contains("utf8-token"));
    }

    fn sealed_provider_config() -> harw_config::ResolvedConfig {
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str::<ProviderToml>(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .expect("valid test provider"),
        );
        config
    }

    fn store_with_test_kek(home: &Path, key_path: &Path) -> SecretStore {
        let policy = CryptoPolicy::strongest();
        let provenance = KekProvenance::KeyFile {
            path: key_path.to_owned(),
        };
        let key_material = load_kek_material(&policy, &provenance).expect("load test KEK material");
        SecretStore::with_key_material(
            home.join("sealed-secrets"),
            policy,
            provenance,
            KeyVersion::initial(),
            key_material,
        )
    }

    fn write_test_kek(home: &Path) -> std::path::PathBuf {
        let path = home.join("test.kek");
        fs::write(&path, b"01234567890123456789012345678901").expect("write test KEK");
        set_private_permissions(&path);
        path
    }

    #[cfg(unix)]
    fn set_private_permissions(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("restrict test KEK permissions");
    }

    #[cfg(not(unix))]
    fn set_private_permissions(_path: &Path) {}
}
