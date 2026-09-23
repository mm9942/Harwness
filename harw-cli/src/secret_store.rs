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

/// Öffnet — falls nötig — den versiegelten Secret-Resolver, ausgewertet nur
/// für den Provider, den dieser Lauf tatsächlich verwendet
/// (`config.harness.default_provider`), statt für jeden aktivierten
/// Provider im ganzen Konfigurationsuniversum.
///
/// # Description
/// [`open_configured_secret_resolver`] verlangt bereits dann ein
/// konfiguriertes KEK, wenn **irgendein** aktivierter Provider eine
/// `secrets:`-Referenz nutzt — auch wenn dieser Lauf ihn nie anspricht. Ein
/// zweiter, für diesen Lauf irrelevanter Provider mit fehlendem KEK würde
/// damit jeden Start blockieren, der gar nicht auf ihn angewiesen ist
/// (dasselbe Muster wie die hängenden Katalog-Referenzen, die
/// `harw_config::ResolvedConfig::validate` inzwischen nicht mehr fatal
/// behandelt). Diese Funktion wertet die KEK-Pflicht stattdessen
/// **verzögert** aus: nur, wenn der tatsächlich gewählte Provider
/// (`config.harness.default_provider`) existiert, aktiviert ist und selbst
/// eine `secrets:`-Referenz trägt. Fehlt `default_provider`, existiert der
/// referenzierte Provider nicht (hängende Referenz), ist er deaktiviert
/// oder nutzt er kein `secrets:`, verlangt diese Funktion kein KEK und öffnet
/// nichts.
///
/// Die No-Fallback-zu-Klartext-Garantie bleibt unverändert: sobald der
/// tatsächlich genutzte Provider `secrets:` referenziert, delegiert diese
/// Funktion an [`open_configured_secret_resolver`] — inklusive dessen
/// Fail-Closed-Verhalten ohne konfiguriertes KEK.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space, siehe [`open_configured_secret_resolver`].
/// - `config` (`&ResolvedConfig`): aufgelöste Konfiguration dieses Laufs.
///
/// # Returns
/// `Some(resolver)`, wenn der tatsächlich verwendete Provider `secrets:`
/// nutzt und der versiegelte Speicher geöffnet werden konnte; `None` sonst
/// — auch dann, wenn ein *anderer*, von diesem Lauf nicht verwendeter
/// Provider `secrets:` nutzen würde.
///
/// # Errors
/// Wie [`open_configured_secret_resolver`]: `String` ohne Geheimnisinhalt,
/// wenn KEK-Konfiguration, KEK-Material oder das Öffnen des Speichers
/// fehlschlägt.
pub fn open_configured_secret_resolver_for_active_provider(
    home: &Path,
    config: &ResolvedConfig,
) -> Result<Option<ConfiguredSecretResolver>, String> {
    let Some(provider_id) = config.harness.default_provider.as_deref() else {
        return Ok(None);
    };
    let Some(provider) = config.providers.get(provider_id) else {
        // Hängende `default_provider`-Referenz: `harw-config` meldet das
        // inzwischen als nicht-fatale Diagnose (`ConfigDiagnostic`), nicht
        // als Startabbruch. Ohne einen echten Provider gibt es hier nichts,
        // das ein KEK verlangen könnte.
        return Ok(None);
    };
    if !(provider.enabled && matches!(&provider.auth, Some(SecretRef::Secrets(_)))) {
        return Ok(None);
    }
    open_configured_secret_resolver(home, config)
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
        open_configured_secret_resolver_for_active_provider,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn no_secrets_provider_reference_returns_none_without_a_kek() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        assert!(
            open_configured_secret_resolver(home.path(), &config)
                .map_err(ctx("no sealed provider must not require a KEK"))?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn sealed_provider_without_a_kek_fails_closed() -> TestResult {
        let config = sealed_provider_config()?;
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let result = open_configured_secret_resolver(home.path(), &config);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "sealed provider without KEK must fail".into(),
            ));
        };
        assert!(error.contains("requires a configured KEK"));
        Ok(())
    }

    #[test]
    fn invalid_keyring_kek_fails_closed_when_sealed_provider_is_configured() -> TestResult {
        let mut config = sealed_provider_config()?;
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::Keyring,
            key_file_path: None,
            keyring_entry: Some("missing-account".to_owned()),
            env_seed_var: None,
        });
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let result = open_configured_secret_resolver(home.path(), &config);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "malformed keyring entry must fail".into(),
            ));
        };
        assert_eq!(error, "keyring KEK entry must be service/account");
        Ok(())
    }

    #[test]
    fn resolver_returns_a_valid_utf8_sealed_secret() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        let mut store = store_with_test_kek(home.path(), &key_path)?;
        store
            .create(
                "provider-token",
                "provider authentication",
                &SecretBox::new(b"utf8-token".to_vec().into_boxed_slice()),
            )
            .map_err(ctx("seal test token"))?;
        let resolver = ConfiguredSecretResolver::new(store);

        let resolved = resolver
            .resolve("provider-token")
            .map_err(ctx("resolve valid UTF-8 secret"))?;
        assert_eq!(resolved.expose_secret(), "utf8-token");
        Ok(())
    }

    #[test]
    fn resolver_rejects_non_utf8_sealed_secret_without_leaking_data() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        let mut store = store_with_test_kek(home.path(), &key_path)?;
        let secret_bytes = [0xff, 0x00, 0x80];
        store
            .create(
                "binary-provider-token",
                "provider authentication",
                &SecretBox::new(secret_bytes.to_vec().into_boxed_slice()),
            )
            .map_err(ctx("seal binary test token"))?;
        let resolver = ConfiguredSecretResolver::new(store);

        let result = resolver.resolve("binary-provider-token");
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "non-UTF-8 secret must be rejected".into(),
            ));
        };
        assert_eq!(error, "sealed secret is not valid UTF-8");
        assert!(!error.contains("255"));
        assert!(!error.contains("binary-provider-token"));
        Ok(())
    }

    #[test]
    fn resolver_reports_a_retired_kem_with_a_migration_hint() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        {
            let mut store = store_with_test_kek(home.path(), &key_path)?;
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"legacy-token".to_vec().into_boxed_slice()),
                )
                .map_err(ctx("seal test token"))?;
        }
        // Reset the durable KEM name to a retired pure ML-KEM level, as written
        // before the hybrid switch.
        let entry = fs::read_dir(home.path().join("sealed-secrets").join("secrets"))
            .map_err(ctx("sealed secrets directory"))?
            .next()
            .ok_or(TestError::Missing("one sealed entry"))?
            .map_err(ctx("sealed entry"))?
            .path();
        let mut durable: serde_json::Value =
            serde_json::from_slice(&fs::read(&entry).map_err(ctx("read sealed entry"))?)
                .map_err(ctx("sealed entry is JSON"))?;
        durable["record"]["kem_algo"] = serde_json::Value::String("ml_kem_768".to_owned());
        let legacy_entry = serde_json::to_vec(&durable).map_err(ctx("encode legacy entry"))?;
        fs::write(&entry, legacy_entry).map_err(ctx("write legacy entry"))?;

        let policy = CryptoPolicy::strongest();
        let provenance = KekProvenance::KeyFile { path: key_path };
        let key_material =
            load_kek_material(&policy, &provenance).map_err(ctx("load test KEK material"))?;
        let store = SecretStore::open_with_key_material(
            home.path().join("sealed-secrets"),
            policy,
            provenance,
            KeyVersion::initial(),
            key_material,
        )
        .map_err(ctx("legacy record still loads"))?;
        let resolver = ConfiguredSecretResolver::new(store);

        let result = resolver.resolve("secrets:provider-token");
        let Err(error) = result else {
            return Err(TestError::Unexpected("retired KEM must not resolve".into()));
        };
        assert!(error.contains("retired KEM 'ml_kem_768'"));
        assert!(error.contains("re-create the secret"));
        assert!(error.contains("docs/setup/crypt-guard.md"));
        assert!(!error.contains("legacy-token"));

        let masked_result = resolver.resolve("secrets:missing-provider-token");
        let Err(masked) = masked_result else {
            return Err(TestError::Unexpected(
                "unknown reference must not resolve".into(),
            ));
        };
        assert_eq!(masked, "sealed secret could not be resolved");
        assert!(!masked.contains("missing-provider-token"));
        Ok(())
    }

    /// Der wichtigste Test dieses Knotens: die periodische Kettenprüfung
    /// bezieht sich auf die Kette des **konfigurierten** `SecretStore` — nicht
    /// auf eine unabhängig davon fabrizierte Kette. Ohne konfigurierten
    /// Speicher gibt es nichts zu prüfen.
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_returns_none_without_a_sealed_provider()
    -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        assert!(
            configured_secret_store_persisted_audit_chain_status(home.path(), &config)
                .map_err(ctx("no sealed provider must not require a KEK"))?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn configured_secret_store_persisted_audit_chain_status_fails_closed_without_a_kek()
    -> TestResult {
        let config = sealed_provider_config()?;
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let result = configured_secret_store_persisted_audit_chain_status(home.path(), &config);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "sealed provider without KEK must fail closed".into(),
            ));
        };
        assert!(error.contains("requires a configured KEK"));
        Ok(())
    }

    /// Der wichtigste Test dieses Knotens: die geprüfte Kette ist die auf der
    /// Platte persistierte `audit.log`-Datei — nicht die im Speicher
    /// gehaltene Kette der frisch geöffneten Instanz (die stets leer wäre,
    /// siehe `SecretStore::open_with_key_material`s Doku). Eine Mutation
    /// durch eine separate `SecretStore`-Instanz an derselben Wurzel muss
    /// hier sichtbar werden, weil sie auf die Platte durchgeschrieben wurde.
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_reads_the_real_disk_file() -> TestResult
    {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        {
            let mut store = store_with_test_kek(home.path(), &key_path)?;
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"utf8-token".to_vec().into_boxed_slice()),
                )
                .map_err(ctx("seal test token"))?;
        }
        let mut config = sealed_provider_config()?;
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });

        let status = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .map_err(ctx("valid KEK must open the configured store"))?
            .ok_or(TestError::Missing("sealed provider is configured"))?
            .map_err(ctx("a freshly written chain must verify intact"))?;

        match status {
            PersistedChainStatus::Intact { event_count, .. } => {
                assert_eq!(
                    event_count, 1,
                    "the disk-persisted chain must show the mutation made by the earlier instance"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an intact persisted chain, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Ohne jede Mutation existiert `audit.log` noch nicht — das ist kein
    /// Fund, sondern „nichts protokolliert".
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_reports_absent_without_mutations()
    -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        let mut config = sealed_provider_config()?;
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });

        let status = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .map_err(ctx("valid KEK must open the configured store"))?
            .ok_or(TestError::Missing("sealed provider is configured"))?;

        assert!(matches!(status, Ok(PersistedChainStatus::Absent)));
        Ok(())
    }

    #[test]
    fn configured_secret_store_persisted_audit_chain_status_never_leaks_secret_content_on_open_failure()
    -> TestResult {
        let mut config = sealed_provider_config()?;
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::Keyring,
            key_file_path: None,
            keyring_entry: Some("missing-account".to_owned()),
            env_seed_var: None,
        });
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let result = configured_secret_store_persisted_audit_chain_status(home.path(), &config);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "malformed keyring entry must fail".into(),
            ));
        };
        assert_eq!(error, "keyring KEK entry must be service/account");
        assert!(!error.contains("provider-token"));
        Ok(())
    }

    /// Ein Lesefehler der persistierten Kette (hier: eine abgeschnittene
    /// Datei) trägt weder Geheimnisinhalt noch Kettenereignisdaten in seiner
    /// `Display`-Meldung.
    #[test]
    fn configured_secret_store_persisted_audit_chain_status_io_error_carries_no_secret_content()
    -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        {
            let mut store = store_with_test_kek(home.path(), &key_path)?;
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"utf8-token".to_vec().into_boxed_slice()),
                )
                .map_err(ctx("seal test token"))?;
        }
        let audit_log_path = home.path().join("sealed-secrets").join("audit.log");
        let bytes = std::fs::read(&audit_log_path).map_err(ctx("read persisted audit log"))?;
        std::fs::write(&audit_log_path, &bytes[..bytes.len() / 2])
            .map_err(ctx("truncate persisted audit log"))?;
        let mut config = sealed_provider_config()?;
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });

        let status = configured_secret_store_persisted_audit_chain_status(home.path(), &config)
            .map_err(ctx("valid KEK must open the configured store"))?
            .ok_or(TestError::Missing("sealed provider is configured"))?;

        let error = match status {
            Err(error @ AuditError::Io(_)) => error,
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an unreadable persisted chain, got {other:?}"
                )));
            }
        };
        assert!(!error.to_string().contains("provider-token"));
        assert!(!error.to_string().contains("utf8-token"));
        Ok(())
    }

    fn sealed_provider_config() -> TestResult<harw_config::ResolvedConfig> {
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str::<ProviderToml>(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("valid test provider"))?,
        );
        Ok(config)
    }

    /// Wie [`sealed_provider_config`], zusätzlich mit einem `env:`-Provider,
    /// der als `default_provider` gewählt wird — der versiegelte `sealed`-
    /// Provider bleibt konfiguriert, aber von diesem Lauf ungenutzt.
    fn config_with_unused_sealed_provider() -> TestResult<harw_config::ResolvedConfig> {
        let mut config = sealed_provider_config()?;
        config.providers.insert(
            "plain".to_owned(),
            toml::from_str::<ProviderToml>(
                "name = \"plain\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"env:PLAIN_TOKEN\"\n",
            )
            .map_err(ctx("valid test provider"))?,
        );
        config.harness.default_provider = Some("plain".to_owned());
        Ok(config)
    }

    #[test]
    fn active_provider_scoped_resolver_ignores_an_unused_sealed_provider_without_a_kek()
    -> TestResult {
        let config = config_with_unused_sealed_provider()?;
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let resolver = open_configured_secret_resolver_for_active_provider(home.path(), &config)
            .map_err(ctx("an unused sealed provider must not require a KEK"))?;
        assert!(resolver.is_none());
        Ok(())
    }

    #[test]
    fn active_provider_scoped_resolver_ignores_a_dangling_default_provider() -> TestResult {
        let mut config = sealed_provider_config()?;
        config.harness.default_provider = Some("missing".to_owned());
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let resolver = open_configured_secret_resolver_for_active_provider(home.path(), &config)
            .map_err(ctx("a dangling default_provider must not require a KEK"))?;
        assert!(resolver.is_none());
        Ok(())
    }

    #[test]
    fn active_provider_scoped_resolver_returns_none_without_any_default_provider() -> TestResult {
        let config = sealed_provider_config()?;
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let resolver = open_configured_secret_resolver_for_active_provider(home.path(), &config)
            .map_err(ctx("no default_provider means nothing is selected yet"))?;
        assert!(resolver.is_none());
        Ok(())
    }

    #[test]
    fn active_provider_scoped_resolver_fails_closed_when_the_active_provider_is_sealed()
    -> TestResult {
        let mut config = sealed_provider_config()?;
        config.harness.default_provider = Some("sealed".to_owned());
        let home = TempDir::new().map_err(ctx("temporary home"))?;

        let result = open_configured_secret_resolver_for_active_provider(home.path(), &config);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "the actually selected sealed provider must still require a KEK".into(),
            ));
        };
        assert!(error.contains("requires a configured KEK"));
        Ok(())
    }

    #[test]
    fn active_provider_scoped_resolver_resolves_when_the_active_provider_is_sealed_and_kek_is_set()
    -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        {
            let mut store = store_with_test_kek(home.path(), &key_path)?;
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"utf8-token".to_vec().into_boxed_slice()),
                )
                .map_err(ctx("seal test token"))?;
        }
        let mut config = sealed_provider_config()?;
        config.harness.default_provider = Some("sealed".to_owned());
        config.auth.kek = Some(KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        });

        let resolver = open_configured_secret_resolver_for_active_provider(home.path(), &config)
            .map_err(ctx("a configured KEK must open the resolver"))?
            .ok_or(TestError::Missing("the active provider uses secrets:"))?;
        let resolved = resolver
            .resolve("provider-token")
            .map_err(ctx("resolve the sealed secret"))?;
        assert_eq!(resolved.expose_secret(), "utf8-token");
        Ok(())
    }

    fn store_with_test_kek(home: &Path, key_path: &Path) -> TestResult<SecretStore> {
        let policy = CryptoPolicy::strongest();
        let provenance = KekProvenance::KeyFile {
            path: key_path.to_owned(),
        };
        let key_material =
            load_kek_material(&policy, &provenance).map_err(ctx("load test KEK material"))?;
        Ok(SecretStore::with_key_material(
            home.join("sealed-secrets"),
            policy,
            provenance,
            KeyVersion::initial(),
            key_material,
        ))
    }

    fn write_test_kek(home: &Path) -> TestResult<std::path::PathBuf> {
        let path = home.join("test.kek");
        fs::write(&path, b"01234567890123456789012345678901").map_err(ctx("write test KEK"))?;
        set_private_permissions(&path)?;
        Ok(path)
    }

    #[cfg(unix)]
    fn set_private_permissions(path: &Path) -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(ctx("restrict test KEK permissions"))?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn set_private_permissions(_path: &Path) -> TestResult {
        Ok(())
    }
}
