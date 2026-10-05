//! CLI composition for configured `secrets:` provider credentials.
//!
//! Reading: the sealed store is opened only when an enabled provider actually
//! needs it (its `auth` or one of its `credential_pool` entries is a
//! `secrets:` reference).
//!
//! Writing: every secret harw itself stores goes through
//! [`SecretStoreWriter`] and comes back as `secrets:<uuid>`. New secrets are
//! sealed as `KmsWrappedV3` through the AuthHub (hub key
//! `harw.secrets-kek/dek-kek`) when `[infrastructure].auth_socket` is set,
//! otherwise as local `DekWrappedV2` records under a KEK that harw bootstraps
//! itself (`<home>/keys/secrets.kek`, recorded as `[kek]` in `auth.toml`)
//! when none is configured. An unreachable hub fails the write; there is no
//! V2 fallback for a configured hub and never a plaintext fallback to the
//! legacy `<home>/secrets` directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_config::{
    AuthConfig, ConfigWriter, InfrastructureSection, KekConfig,
    KekProvenance as ConfigKekProvenance, ProviderToml, ResolvedConfig, SecretRef,
};
use harw_infra_client::{
    AuthHubClient, AuthHubDekWrapper, CreatedKey, InfraClientError, InfrastructureAvailability,
    KeyDescription, KeyProfile, KeyRef, KeyState, RemoteErrorKind,
};
use harw_provider_http::SecretResolver;
use harw_secrets::audit::chain::PersistedChainStatus;
use harw_secrets::{
    AuditResult, CryptoPolicy, KekProvenance, KeyVersion, SecretEnvelopeFormat, SecretStore,
    SecretsError, load_kek_material,
};
use secrecy::SecretString;
use secrecy_08::ExposeSecret as _;

/// AuthHub namespace of the secrets KEK (`HarwKeyPurpose::SecretsKek`).
pub(crate) const SECRETS_KEK_NAMESPACE: &str = "harw.secrets-kek";

/// AuthHub key id of the DEK-wrapping KEK inside [`SECRETS_KEK_NAMESPACE`].
pub(crate) const SECRETS_KEK_KEY_ID: &str = "dek-kek";

/// Root of the sealed store below `<home>`, distinct from the legacy
/// plaintext `<home>/secrets` directory.
const SEALED_STORE_DIR: &str = "sealed-secrets";

/// Directory below `<home>` for the KEK harw bootstraps itself.
const BOOTSTRAP_KEK_DIR: &str = "keys";

/// File name of the bootstrapped key-file KEK inside [`BOOTSTRAP_KEK_DIR`].
const BOOTSTRAP_KEK_FILE: &str = "secrets.kek";

/// Name of the short-lived thread that talks to the AuthHub while a writer
/// is opened.
const HUB_BRIDGE_THREAD_NAME: &str = "harw-secrets-kek";

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
/// distinct from the legacy plaintext `<home>/secrets` directory. Nothing is
/// opened unless configuration proves that at least one enabled provider has
/// a `secrets:` reference, either as `auth` or as a `credential_pool` entry.
///
/// Without a hub the configured KEK is required and V2 records resolve
/// exactly as before. With `[infrastructure].auth_socket` the store
/// additionally gets the AuthHub DEK wrapper so V3 records open; the KEK is
/// then optional (without it, V2 records fail closed). The wrapper's
/// generation is taken from the V3 records already in the store, so opening
/// makes no network call.
pub fn open_configured_secret_resolver(
    home: &Path,
    config: &ResolvedConfig,
) -> Result<Option<ConfiguredSecretResolver>, String> {
    if !configured_provider_uses_sealed_secret(config) {
        return Ok(None);
    }

    let store = match select_secret_sealing(config)? {
        SecretSealing::LocalV2 => {
            let kek = config.auth.kek.as_ref().ok_or_else(|| {
                "enabled sealed-secret provider requires a configured KEK".to_owned()
            })?;
            open_store_with_kek(home, kek)?
        }
        SecretSealing::AuthHubV3 => open_hub_reader_store(home, config)?,
    };

    Ok(Some(ConfiguredSecretResolver::new(store)))
}

/// Opens the sealed store with the local KEK `kek` (V1/V2 records).
fn open_store_with_kek(home: &Path, kek: &KekConfig) -> Result<SecretStore, String> {
    let provenance = configured_kek_provenance(kek)?;
    let policy = CryptoPolicy::strongest();
    let key_material = load_kek_material(&policy, &provenance)
        .map_err(|_| "sealed secret resolver could not load KEK material".to_owned())?;
    SecretStore::open_with_key_material(
        home.join(SEALED_STORE_DIR),
        policy,
        provenance,
        KeyVersion::initial(),
        key_material,
    )
    .map_err(|_| "sealed secret resolver could not open sealed secret store".to_owned())
}

/// Opens the sealed store for reading with the AuthHub DEK wrapper attached.
///
/// A configured KEK is loaded as well so V2 records keep resolving; without
/// one the store is opened with the nominal key-file provenance and V2 reads
/// fail closed. No network call happens here.
fn open_hub_reader_store(home: &Path, config: &ResolvedConfig) -> Result<SecretStore, String> {
    let section = config
        .infrastructure
        .as_ref()
        .ok_or_else(|| "AuthHub sealing requires an [infrastructure] section".to_owned())?;
    let client = authhub_client(section)?;
    let store = match config.auth.kek.as_ref() {
        Some(kek) => open_store_with_kek(home, kek)?,
        None => SecretStore::open(
            home.join(SEALED_STORE_DIR),
            CryptoPolicy::strongest(),
            nominal_kek_provenance(home),
            KeyVersion::initial(),
        )
        .map_err(|_| "sealed secret resolver could not open sealed secret store".to_owned())?,
    };
    let generation = v3_reader_generation(&store);
    let wrapper = AuthHubDekWrapper::new(client, &secrets_kek_ref()?, generation)
        .map_err(|_| "AuthHub DEK wrapper could not be configured".to_owned())?;
    Ok(store.with_dek_wrapper(Arc::new(wrapper)))
}

/// How new secrets are sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecretSealing {
    /// Local `DekWrappedV2` record under a KEK from `[kek]` (bootstrapped by
    /// harw when none is configured).
    LocalV2,
    /// `KmsWrappedV3` record whose DEK the AuthHub wraps.
    AuthHubV3,
}

/// Chooses the sealing for new secrets from `[infrastructure]`. Pure, no I/O.
///
/// # Errors
/// `token_file` without `auth_socket`: the configuration is ambiguous, so no
/// sealing is chosen (and nothing is written).
pub(crate) fn select_secret_sealing(config: &ResolvedConfig) -> Result<SecretSealing, String> {
    let Some(section) = config.infrastructure.as_ref() else {
        return Ok(SecretSealing::LocalV2);
    };
    match (&section.auth_socket, &section.token_file) {
        (Some(_), _) => Ok(SecretSealing::AuthHubV3),
        (None, Some(_)) => Err(
            "[infrastructure] token_file requires auth_socket; refusing to choose a secret sealing"
                .to_owned(),
        ),
        (None, None) => Ok(SecretSealing::LocalV2),
    }
}

/// A sealed store opened for writing new secrets.
pub(crate) struct SecretStoreWriter {
    store: SecretStore,
}

/// Opens the sealed store for writing, with the sealing
/// [`select_secret_sealing`] chooses.
///
/// - AuthHub: the hub key `harw.secrets-kek/dek-kek` is described (and
///   created once if the hub reports it missing); an unreachable hub or any
///   other hub error fails here. There is never a V2 fallback.
/// - Local: the configured KEK is used; without one harw bootstraps a
///   key-file KEK (see [`ensure_local_kek`]).
///
/// # Errors
/// A `String` naming references or paths, never secret bytes.
pub(crate) fn open_secret_store_for_writing(
    home: &Path,
    config: &ResolvedConfig,
) -> Result<SecretStoreWriter, String> {
    let store = match select_secret_sealing(config)? {
        SecretSealing::AuthHubV3 => {
            let section = config
                .infrastructure
                .as_ref()
                .ok_or_else(|| "AuthHub sealing requires an [infrastructure] section".to_owned())?;
            let client = authhub_client(section)?;
            let key = secrets_kek_ref()?;
            let generation = ensure_hub_key_generation(&client, &key)?;
            let store = SecretStore::open(
                home.join(SEALED_STORE_DIR),
                CryptoPolicy::strongest(),
                nominal_kek_provenance(home),
                KeyVersion::initial(),
            )
            .map_err(|_| "sealed secret store could not be opened".to_owned())?;
            let wrapper = AuthHubDekWrapper::new(client, &key, generation)
                .map_err(|_| "AuthHub DEK wrapper could not be configured".to_owned())?;
            store.with_dek_wrapper(Arc::new(wrapper))
        }
        SecretSealing::LocalV2 => {
            let kek = ensure_local_kek(home, config)?;
            open_store_with_kek(home, &kek)?
        }
    };
    Ok(SecretStoreWriter { store })
}

impl SecretStoreWriter {
    /// Seals `value` as a new secret and returns its `secrets:<uuid>`
    /// reference.
    ///
    /// # Errors
    /// An empty or whitespace-only value, or a failed seal/persist. The
    /// message never contains the value.
    pub(crate) fn store(
        &mut self,
        name: &str,
        purpose: &str,
        value: &SecretString,
    ) -> Result<SecretRef, String> {
        let exposed = secrecy::ExposeSecret::expose_secret(value);
        if exposed.trim().is_empty() {
            return Err("refusing to store an empty secret".to_owned());
        }
        let boxed = secrecy_08::SecretBox::new(exposed.as_bytes().to_vec().into_boxed_slice());
        let id = self
            .store
            .create(name, purpose, &boxed)
            .map_err(store_error_message)?;
        Ok(SecretRef::Secrets(id.to_string()))
    }
}

/// Opens the store for writing and seals `value` once, see
/// [`open_secret_store_for_writing`] and [`SecretStoreWriter::store`].
pub(crate) fn store_secret(
    home: &Path,
    config: &ResolvedConfig,
    name: &str,
    purpose: &str,
    value: &SecretString,
) -> Result<SecretRef, String> {
    open_secret_store_for_writing(home, config)?.store(name, purpose, value)
}

/// Maps a store error on create to a message without secret content. The
/// DEK wrapper's reason is payload-free and is kept.
fn store_error_message(error: SecretsError) -> String {
    match error {
        SecretsError::DekWrapperUnavailable { reason, .. } => {
            format!("sealed secret could not be stored: {reason}")
        }
        _ => "sealed secret could not be stored".to_owned(),
    }
}

/// Nominal provenance of a store whose writes go through the AuthHub. It is
/// never loaded; V1/V2 reads through such a store fail closed.
fn nominal_kek_provenance(home: &Path) -> KekProvenance {
    KekProvenance::KeyFile {
        path: home.join(BOOTSTRAP_KEK_DIR).join(BOOTSTRAP_KEK_FILE),
    }
}

/// Builds the AuthHub client from `[infrastructure]`. Opens no socket.
pub(crate) fn authhub_client(section: &InfrastructureSection) -> Result<AuthHubClient, String> {
    let availability = InfrastructureAvailability::from_config(
        &harw_runtime::infrastructure::client_config(section),
    )
    .map_err(|error| format!("[infrastructure] is invalid: {error}"))?;
    availability
        .auth
        .ok_or_else(|| "[infrastructure] configures no auth_socket".to_owned())
}

/// Latest version of the hub key `harw.secrets-kek/dek-kek`.
fn secrets_kek_ref() -> Result<KeyRef, String> {
    KeyRef::latest(SECRETS_KEK_NAMESPACE, SECRETS_KEK_KEY_ID).map_err(|error| {
        format!("AuthHub key {SECRETS_KEK_NAMESPACE}/{SECRETS_KEK_KEY_ID} is invalid: {error}")
    })
}

/// Determines the 0-based generation of the hub key `key`, creating the key
/// once when the hub reports it missing.
///
/// The async client runs on one short-lived thread with its own
/// current-thread runtime, so this works from plain threads and from inside
/// a tokio runtime alike.
///
/// # Errors
/// Any hub error other than a missing key (unavailable, timeout,
/// unauthenticated, …), a key that is not enabled or has another profile,
/// and a failed bridge thread. Never falls back to local sealing.
fn ensure_hub_key_generation(client: &AuthHubClient, key: &KeyRef) -> Result<u32, String> {
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name(HUB_BRIDGE_THREAD_NAME.to_owned())
            .spawn_scoped(scope, move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "AuthHub bridge thread failed".to_owned())?;
                runtime.block_on(hub_key_generation(client, key))
            });
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|_| Err("AuthHub bridge thread failed".to_owned())),
            Err(_) => Err("AuthHub bridge thread failed".to_owned()),
        }
    })
}

/// The two hub key calls [`hub_key_generation`] needs. A seam so the
/// describe/generate decision is testable without a hub; production uses
/// [`AuthHubClient`].
trait HubKeys {
    /// See [`AuthHubClient::describe`].
    async fn describe(&self, key: &KeyRef) -> Result<KeyDescription, InfraClientError>;

    /// See [`AuthHubClient::generate_key`].
    async fn generate_key(
        &self,
        key: &KeyRef,
        profile: KeyProfile,
    ) -> Result<CreatedKey, InfraClientError>;
}

impl HubKeys for AuthHubClient {
    async fn describe(&self, key: &KeyRef) -> Result<KeyDescription, InfraClientError> {
        AuthHubClient::describe(self, key).await
    }

    async fn generate_key(
        &self,
        key: &KeyRef,
        profile: KeyProfile,
    ) -> Result<CreatedKey, InfraClientError> {
        AuthHubClient::generate_key(self, key, profile).await
    }
}

/// Describe, and on not-found generate exactly once (a `409` from generate
/// means a concurrent creation: describe once more).
async fn hub_key_generation<C: HubKeys>(client: &C, key: &KeyRef) -> Result<u32, String> {
    match client.describe(key).await {
        Ok(description) => described_generation(&description),
        Err(InfraClientError::NotFound) => {
            match client.generate_key(key, KeyProfile::PqHpkeDefault).await {
                Ok(created) => generation_of_version(created.key.version()),
                Err(InfraClientError::Remote(RemoteErrorKind::Conflict)) => {
                    match client.describe(key).await {
                        Ok(description) => described_generation(&description),
                        Err(error) => Err(hub_key_unavailable(error)),
                    }
                }
                Err(error) => Err(format!(
                    "AuthHub key {SECRETS_KEK_NAMESPACE}/{SECRETS_KEK_KEY_ID} could not be created: {error}"
                )),
            }
        }
        Err(error) => Err(hub_key_unavailable(error)),
    }
}

/// Checks a described hub key (enabled, `pq-hpke-default`) and returns its
/// 0-based generation.
fn described_generation(description: &KeyDescription) -> Result<u32, String> {
    if description.state != KeyState::Enabled {
        return Err(format!(
            "AuthHub key {SECRETS_KEK_NAMESPACE}/{SECRETS_KEK_KEY_ID} is not enabled"
        ));
    }
    if description.profile() != Some(KeyProfile::PqHpkeDefault) {
        return Err(format!(
            "AuthHub key {SECRETS_KEK_NAMESPACE}/{SECRETS_KEK_KEY_ID} does not use the pq-hpke-default profile"
        ));
    }
    generation_of_version(description.key.version())
}

/// Hub key version (1-based) to Harwness generation (0-based).
fn generation_of_version(version: Option<u32>) -> Result<u32, String> {
    version
        .and_then(|version| version.checked_sub(1))
        .ok_or_else(|| {
            format!(
                "AuthHub key {SECRETS_KEK_NAMESPACE}/{SECRETS_KEK_KEY_ID} reported no key version"
            )
        })
}

fn hub_key_unavailable(error: InfraClientError) -> String {
    format!("AuthHub key {SECRETS_KEK_NAMESPACE}/{SECRETS_KEK_KEY_ID} unavailable: {error}")
}

/// Highest generation of `harw.secrets-kek/dek-kek` among the V3 records
/// already in `store`, `0` when there is none. Pure: the reader opens
/// without dialling the hub.
fn v3_reader_generation(store: &SecretStore) -> u32 {
    let key_id = format!("{SECRETS_KEK_NAMESPACE}/{SECRETS_KEK_KEY_ID}");
    store
        .list()
        .iter()
        .filter_map(|metadata| store.record(&metadata.id).ok())
        .filter(|record| {
            record.envelope_format == SecretEnvelopeFormat::KmsWrappedV3
                && record.key_id.as_deref() == Some(key_id.as_str())
        })
        .filter_map(|record| record.key_generation)
        .max()
        .unwrap_or(0)
}

/// Returns the local KEK for writing, bootstrapping one when none exists.
///
/// Order: the resolved `[kek]`; a `[kek]` read fresh from the active
/// profile's or the root `auth.toml` (the resolved config may be stale within
/// one process); otherwise a new key file `<home>/keys/secrets.kek` (32
/// random bytes, `0600`, directory `0700`), recorded as
/// `[kek] provenance = "key_file"` in `<home>/auth.toml` and, if it exists,
/// in the active profile's `auth.toml`. An existing `[kek]` or key file is
/// never overwritten.
///
/// # Errors
/// Unreadable or invalid `auth.toml`, a failed bootstrap or config write, or
/// a platform without key-file KEK support. Messages name paths only.
fn ensure_local_kek(home: &Path, config: &ResolvedConfig) -> Result<KekConfig, String> {
    if let Some(kek) = config.auth.kek.as_ref() {
        return Ok(kek.clone());
    }

    let root_auth = harw_home::auth_path(home);
    let profile_auth = active_profile_auth_path(home)?;
    if let Some(kek) = read_auth_kek(&profile_auth)? {
        return Ok(kek);
    }
    if let Some(kek) = read_auth_kek(&root_auth)? {
        // A profile `auth.toml` replaces the root one as a whole, so the
        // root `[kek]` only takes effect there once it is recorded as well.
        if profile_auth.exists() {
            write_kek_section(&profile_auth, &kek)?;
        }
        return Ok(kek);
    }

    let key_path = bootstrap_key_file(home)?;
    let kek = KekConfig {
        provenance: ConfigKekProvenance::KeyFile,
        key_file_path: Some(key_path.to_string_lossy().into_owned()),
        keyring_entry: None,
        env_seed_var: None,
    };
    write_kek_section(&root_auth, &kek)?;
    if profile_auth.exists() {
        write_kek_section(&profile_auth, &kek)?;
    }
    Ok(kek)
}

/// `auth.toml` of the active profile (it may not exist).
fn active_profile_auth_path(home: &Path) -> Result<PathBuf, String> {
    let name = harw_home::active_profile_name(home);
    harw_home::profile_dir(home, &name)
        .map(|dir| harw_home::auth_path(&dir))
        .map_err(|_| format!("active profile '{name}' has an invalid name"))
}

/// `[kek]` of the `auth.toml` at `path`, `None` when the file or the table
/// is missing.
fn read_auth_kek(path: &Path) -> Result<Option<KekConfig>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(format!("could not read '{}'", path.display())),
    };
    let auth: AuthConfig = toml::from_str(&content)
        .map_err(|_| format!("'{}' is not a valid auth.toml", path.display()))?;
    Ok(auth.kek)
}

/// Records `kek` as `[kek]` in the `auth.toml` at `path` through
/// [`ConfigWriter`], unless a `[kek]` provenance is already there.
fn write_kek_section(path: &Path, kek: &KekConfig) -> Result<(), String> {
    let write_error = || format!("could not record [kek] in '{}'", path.display());
    let mut writer = ConfigWriter::open(path).map_err(|_| write_error())?;
    if writer.get_value("kek.provenance").is_some() {
        return Ok(());
    }
    let provenance = match kek.provenance {
        ConfigKekProvenance::KeyFile => "key_file",
        ConfigKekProvenance::Keyring => "keyring",
        ConfigKekProvenance::EnvSeed => "env_seed",
    };
    writer
        .set_value("kek.provenance", toml_edit::value(provenance))
        .map_err(|_| write_error())?;
    let fields = [
        ("kek.key_file_path", kek.key_file_path.as_deref()),
        ("kek.keyring_entry", kek.keyring_entry.as_deref()),
        ("kek.env_seed_var", kek.env_seed_var.as_deref()),
    ];
    for (key, field) in fields
        .into_iter()
        .filter_map(|(key, field)| field.map(|field| (key, field)))
    {
        writer
            .set_value(key, toml_edit::value(field))
            .map_err(|_| write_error())?;
    }
    writer.save().map_err(|_| write_error())
}

/// Creates `<home>/keys/secrets.kek` (32 random bytes, `0600`) below a real
/// `0700` directory, or reuses an existing key file without touching it.
/// Returns the absolute key path.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn bootstrap_key_file(home: &Path) -> Result<PathBuf, String> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let keys_dir = home.join(BOOTSTRAP_KEK_DIR);
    let dir_error = || format!("could not prepare KEK directory '{}'", keys_dir.display());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&keys_dir)
        .map_err(|_| dir_error())?;
    let metadata = std::fs::symlink_metadata(&keys_dir).map_err(|_| dir_error())?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "KEK directory '{}' is not a real directory",
            keys_dir.display()
        ));
    }
    std::fs::set_permissions(&keys_dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| dir_error())?;
    let keys_dir = std::fs::canonicalize(&keys_dir).map_err(|_| dir_error())?;
    let key_path = keys_dir.join(BOOTSTRAP_KEK_FILE);

    match std::fs::symlink_metadata(&key_path) {
        Ok(_) => return Ok(key_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(format!(
                "could not inspect key file '{}'",
                key_path.display()
            ));
        }
    }
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(key_path),
        Err(_) => {
            return Err(format!(
                "could not create key file '{}'",
                key_path.display()
            ));
        }
    };

    let mut seed = secrecy::SecretBox::new(Box::new([0u8; 32]));
    let written = getrandom::fill(secrecy::ExposeSecretMut::expose_secret_mut(&mut seed))
        .map_err(|_| "the system random source failed".to_owned())
        .and_then(|()| {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .and_then(|()| file.write_all(secrecy::ExposeSecret::expose_secret(&seed)))
                .and_then(|()| file.sync_all())
                .map_err(|_| format!("could not write key file '{}'", key_path.display()))
        });
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&key_path);
        return Err(error);
    }
    std::fs::File::open(&keys_dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| dir_error())?;
    Ok(key_path)
}

/// Key-file KEKs are only supported where `harw-secrets` can open them
/// safely; elsewhere nothing is created.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn bootstrap_key_file(_home: &Path) -> Result<PathBuf, String> {
    Err("no [kek] configured and a key-file KEK is not supported on this platform; configure [kek] provenance keyring or env_seed in auth.toml".to_owned())
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
/// eine `secrets:`-Referenz trägt — als `auth` oder als Eintrag seines
/// `credential_pool` (auch ein Pool-Eintrag zählt, siehe
/// [`provider_uses_sealed_secret`]). Fehlt `default_provider`, existiert der
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
    if !provider_uses_sealed_secret(provider_id, provider, &config.auth) {
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
/// `secrets:`-Referenz nutzt und ein KEK oder ein AuthHub konfiguriert ist),
/// dieselbe KEK-Auflösung, derselbe Pfad — und ruft anschließend
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
    config.providers.iter().any(|(provider_id, provider)| {
        provider_uses_sealed_secret(provider_id, provider, &config.auth)
    })
}

/// `true` when `provider` is enabled and needs the sealed store: its `auth`
/// or one of its `credential_pool` entries is a `secrets:` reference.
pub(crate) fn provider_uses_sealed_secret(
    provider_id: &str,
    provider: &ProviderToml,
    auth: &AuthConfig,
) -> bool {
    provider.enabled
        && (matches!(&provider.auth, Some(SecretRef::Secrets(_)))
            || auth
                .credential_pool
                .get(provider_id)
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| matches!(entry.secret, SecretRef::Secrets(_)))
                }))
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
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;
    use std::path::{Path, PathBuf};

    use harw_config::{
        CredentialEntry, InfrastructureSection, KekConfig, KekProvenance as ConfigKekProvenance,
        ProviderToml, SecretRef,
    };
    use harw_secrets::audit::chain::PersistedChainStatus;
    use harw_secrets::{
        AuditError, CryptoPolicy, KekProvenance, KeyVersion, SecretStore, load_kek_material,
    };
    use secrecy::{ExposeSecret as _, SecretString};
    use secrecy_08::SecretBox;
    use tempfile::TempDir;

    use harw_infra_client::{
        CreatedKey, InfraClientError, KeyDescription, KeyProfile, KeyRef, KeyState, RemoteErrorKind,
    };

    use super::{
        ConfiguredSecretResolver, HubKeys, SECRETS_KEK_KEY_ID, SECRETS_KEK_NAMESPACE,
        SecretResolver, SecretSealing, configured_secret_store_persisted_audit_chain_status,
        hub_key_generation, open_configured_secret_resolver,
        open_configured_secret_resolver_for_active_provider, secrets_kek_ref,
        select_secret_sealing, store_secret,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    /// Scripted hub: answers `describe` from a queue and `generate_key` once,
    /// and records every call.
    struct FakeHub {
        describes: RefCell<VecDeque<Result<KeyDescription, InfraClientError>>>,
        generated: RefCell<Option<Result<CreatedKey, InfraClientError>>>,
        describe_calls: Cell<usize>,
        generate_calls: RefCell<Vec<KeyProfile>>,
    }

    impl FakeHub {
        fn new(
            describes: Vec<Result<KeyDescription, InfraClientError>>,
            generated: Option<Result<CreatedKey, InfraClientError>>,
        ) -> Self {
            Self {
                describes: RefCell::new(describes.into()),
                generated: RefCell::new(generated),
                describe_calls: Cell::new(0),
                generate_calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl HubKeys for FakeHub {
        async fn describe(&self, _key: &KeyRef) -> Result<KeyDescription, InfraClientError> {
            self.describe_calls.set(self.describe_calls.get() + 1);
            self.describes
                .borrow_mut()
                .pop_front()
                .unwrap_or(Err(InfraClientError::Protocol("unexpected describe")))
        }

        async fn generate_key(
            &self,
            _key: &KeyRef,
            profile: KeyProfile,
        ) -> Result<CreatedKey, InfraClientError> {
            self.generate_calls.borrow_mut().push(profile);
            self.generated
                .borrow_mut()
                .take()
                .unwrap_or(Err(InfraClientError::Protocol("unexpected generate")))
        }
    }

    fn hub_key_version(version: u32) -> TestResult<KeyRef> {
        KeyRef::versioned(SECRETS_KEK_NAMESPACE, SECRETS_KEK_KEY_ID, version)
            .map_err(ctx("versioned hub key"))
    }

    fn hub_description(state: KeyState, version: u32) -> TestResult<KeyDescription> {
        Ok(KeyDescription {
            key: hub_key_version(version)?,
            state,
            profile_name: Some(KeyProfile::PqHpkeDefault.wire_name().to_owned()),
        })
    }

    #[tokio::test]
    async fn hub_key_generation_creates_a_missing_key_once() -> TestResult {
        let hub = FakeHub::new(
            vec![Err(InfraClientError::NotFound)],
            Some(Ok(CreatedKey {
                key: hub_key_version(1)?,
                public: None,
            })),
        );
        let key = secrets_kek_ref().map_err(ctx("hub key reference"))?;

        let generation = hub_key_generation(&hub, &key)
            .await
            .map_err(ctx("a missing key is created"))?;

        assert_eq!(generation, 0);
        assert_eq!(hub.describe_calls.get(), 1);
        assert_eq!(
            *hub.generate_calls.borrow(),
            vec![KeyProfile::PqHpkeDefault]
        );
        Ok(())
    }

    #[tokio::test]
    async fn hub_key_generation_redescribes_after_a_generate_conflict() -> TestResult {
        let hub = FakeHub::new(
            vec![
                Err(InfraClientError::NotFound),
                Ok(hub_description(KeyState::Enabled, 4)?),
            ],
            Some(Err(InfraClientError::Remote(RemoteErrorKind::Conflict))),
        );
        let key = secrets_kek_ref().map_err(ctx("hub key reference"))?;

        let generation = hub_key_generation(&hub, &key)
            .await
            .map_err(ctx("a concurrent creation is described again"))?;

        assert_eq!(generation, 3);
        assert_eq!(hub.describe_calls.get(), 2);
        assert_eq!(
            *hub.generate_calls.borrow(),
            vec![KeyProfile::PqHpkeDefault]
        );
        Ok(())
    }

    #[tokio::test]
    async fn hub_key_generation_reports_a_failed_creation() -> TestResult {
        let hub = FakeHub::new(
            vec![Err(InfraClientError::NotFound)],
            Some(Err(InfraClientError::Unavailable)),
        );
        let key = secrets_kek_ref().map_err(ctx("hub key reference"))?;

        let Err(error) = hub_key_generation(&hub, &key).await else {
            return Err(TestError::Unexpected(
                "a failed generate must fail the key lookup".into(),
            ));
        };

        assert!(error.contains("could not be created"));
        assert!(error.contains("harw.secrets-kek/dek-kek"));
        assert_eq!(hub.describe_calls.get(), 1);
        assert_eq!(hub.generate_calls.borrow().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn hub_key_generation_rejects_a_disabled_key_without_generating() -> TestResult {
        let hub = FakeHub::new(vec![Ok(hub_description(KeyState::Disabled, 1)?)], None);
        let key = secrets_kek_ref().map_err(ctx("hub key reference"))?;

        let Err(error) = hub_key_generation(&hub, &key).await else {
            return Err(TestError::Unexpected(
                "a disabled hub key must not be used".into(),
            ));
        };

        assert!(error.contains("is not enabled"));
        assert!(error.contains("harw.secrets-kek/dek-kek"));
        assert!(hub.generate_calls.borrow().is_empty());
        Ok(())
    }

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

    #[test]
    fn select_secret_sealing_table() -> TestResult {
        let socket = || Some(PathBuf::from("/run/harw/infra/secure.sock"));
        let token = || Some(PathBuf::from("/run/harw/infra/token"));
        let cases = [
            (None, Some(SecretSealing::LocalV2)),
            (
                Some(InfrastructureSection {
                    network_socket: Some(PathBuf::from("/run/harw/infra/network.sock")),
                    ..InfrastructureSection::default()
                }),
                Some(SecretSealing::LocalV2),
            ),
            (
                Some(InfrastructureSection {
                    auth_socket: socket(),
                    ..InfrastructureSection::default()
                }),
                Some(SecretSealing::AuthHubV3),
            ),
            (
                Some(InfrastructureSection {
                    auth_socket: socket(),
                    token_file: token(),
                    ..InfrastructureSection::default()
                }),
                Some(SecretSealing::AuthHubV3),
            ),
            (
                Some(InfrastructureSection {
                    token_file: token(),
                    ..InfrastructureSection::default()
                }),
                None,
            ),
        ];
        for (infrastructure, expected) in cases {
            let config = harw_config::ResolvedConfig {
                infrastructure,
                ..harw_config::ResolvedConfig::default()
            };
            let actual = select_secret_sealing(&config);
            match expected {
                Some(expected) => {
                    assert_eq!(actual.map_err(ctx("sealing is selectable"))?, expected);
                }
                None => {
                    let Err(error) = actual else {
                        return Err(TestError::Unexpected(
                            "token_file without auth_socket must not select a sealing".into(),
                        ));
                    };
                    assert!(error.contains("token_file requires auth_socket"));
                }
            }
        }
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn store_secret_bootstraps_kek_and_round_trips() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let config = harw_config::ResolvedConfig::default();
        let value = SecretString::from("sk-bootstrap-value".to_owned());

        let reference = store_secret(
            home.path(),
            &config,
            "provider-openai",
            "provider-auth",
            &value,
        )
        .map_err(ctx("store the secret with a bootstrapped KEK"))?;
        let SecretRef::Secrets(id) = &reference else {
            return Err(TestError::Unexpected(format!(
                "expected a secrets: reference, got {reference}"
            )));
        };
        harw_secrets::SecretId::parse(id).map_err(ctx("secret id is a UUID"))?;

        let keys_dir = home.path().join("keys");
        let key_path = keys_dir.join("secrets.kek");
        let key_metadata = fs::metadata(&key_path).map_err(ctx("bootstrapped key file"))?;
        assert_eq!(key_metadata.len(), 32);
        assert_eq!(key_metadata.permissions().mode() & 0o777, 0o600);
        let dir_metadata = fs::metadata(&keys_dir).map_err(ctx("bootstrapped key directory"))?;
        assert_eq!(dir_metadata.permissions().mode() & 0o777, 0o700);

        let auth_path = home.path().join("auth.toml");
        let auth_before = fs::read_to_string(&auth_path).map_err(ctx("auth.toml was written"))?;
        let auth: harw_config::AuthConfig =
            toml::from_str(&auth_before).map_err(ctx("auth.toml parses"))?;
        let kek = auth.kek.ok_or(TestError::Missing("[kek] was recorded"))?;
        assert!(matches!(kek.provenance, ConfigKekProvenance::KeyFile));
        let canonical_key = fs::canonicalize(&key_path)
            .map_err(ctx("canonical key path"))?
            .to_string_lossy()
            .into_owned();
        assert_eq!(kek.key_file_path.as_deref(), Some(canonical_key.as_str()));

        let mut read_config = harw_config::ResolvedConfig {
            auth: harw_config::AuthConfig {
                kek: Some(kek),
                ..harw_config::AuthConfig::default()
            },
            ..harw_config::ResolvedConfig::default()
        };
        read_config.providers.insert(
            "openai".to_owned(),
            provider_with_auth(&reference.to_string())?,
        );
        let resolver = open_configured_secret_resolver(home.path(), &read_config)
            .map_err(ctx("the recorded KEK opens the store"))?
            .ok_or(TestError::Missing("provider uses secrets:"))?;
        let resolved = resolver
            .resolve(&reference.to_string())
            .map_err(ctx("resolve the stored secret"))?;
        assert_eq!(resolved.expose_secret(), "sk-bootstrap-value");

        let key_before = fs::read(&key_path).map_err(ctx("read key file"))?;
        store_secret(
            home.path(),
            &config,
            "provider-openai-second",
            "provider-auth",
            &SecretString::from("sk-second-value".to_owned()),
        )
        .map_err(ctx("a stale config reuses the recorded KEK"))?;
        assert_eq!(
            fs::read(&key_path).map_err(ctx("re-read key file"))?,
            key_before
        );
        assert_eq!(
            fs::read_to_string(&auth_path).map_err(ctx("re-read auth.toml"))?,
            auth_before
        );
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn existing_key_file_is_never_overwritten() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let keys_dir = home.path().join("keys");
        fs::create_dir(&keys_dir).map_err(ctx("create key directory"))?;
        fs::set_permissions(&keys_dir, fs::Permissions::from_mode(0o700))
            .map_err(ctx("restrict key directory"))?;
        let key_path = keys_dir.join("secrets.kek");
        let existing = [0x5A_u8; 32];
        fs::write(&key_path, existing).map_err(ctx("write existing key file"))?;
        set_private_permissions(&key_path)?;

        store_secret(
            home.path(),
            &harw_config::ResolvedConfig::default(),
            "provider-openai",
            "provider-auth",
            &SecretString::from("sk-existing-key-value".to_owned()),
        )
        .map_err(ctx("store under the existing key file"))?;

        assert_eq!(
            fs::read(&key_path).map_err(ctx("re-read key file"))?,
            existing.to_vec()
        );
        let auth: harw_config::AuthConfig = toml::from_str(
            &fs::read_to_string(home.path().join("auth.toml")).map_err(ctx("read auth.toml"))?,
        )
        .map_err(ctx("auth.toml parses"))?;
        let kek = auth.kek.ok_or(TestError::Missing("[kek] was recorded"))?;
        assert!(matches!(kek.provenance, ConfigKekProvenance::KeyFile));
        let canonical_key = fs::canonicalize(&key_path)
            .map_err(ctx("canonical key path"))?
            .to_string_lossy()
            .into_owned();
        assert_eq!(kek.key_file_path.as_deref(), Some(canonical_key.as_str()));
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn existing_kek_in_auth_toml_is_kept() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        let auth_path = home.path().join("auth.toml");
        let auth_before = format!(
            "[kek]\nprovenance = \"key_file\"\nkey_file_path = \"{}\"\n",
            key_path.display()
        );
        fs::write(&auth_path, &auth_before).map_err(ctx("write auth.toml"))?;

        let reference = store_secret(
            home.path(),
            &harw_config::ResolvedConfig::default(),
            "provider-openai",
            "provider-auth",
            &SecretString::from("sk-configured-kek-value".to_owned()),
        )
        .map_err(ctx("store under the configured KEK"))?;

        assert!(matches!(reference, SecretRef::Secrets(_)));
        assert!(!home.path().join("keys").exists());
        assert_eq!(
            fs::read_to_string(&auth_path).map_err(ctx("re-read auth.toml"))?,
            auth_before
        );
        Ok(())
    }

    #[test]
    fn unreachable_hub_fails_closed_without_record() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let config = harw_config::ResolvedConfig {
            infrastructure: Some(InfrastructureSection {
                auth_socket: Some(home.path().join("missing.sock")),
                ..InfrastructureSection::default()
            }),
            ..harw_config::ResolvedConfig::default()
        };

        let result = store_secret(
            home.path(),
            &config,
            "provider-openai",
            "provider-auth",
            &SecretString::from("sk-unreachable-value".to_owned()),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "an unreachable hub must fail the write".into(),
            ));
        };
        assert!(error.contains("harw.secrets-kek/dek-kek"));
        assert!(!error.contains("sk-unreachable-value"));
        assert!(!home.path().join("sealed-secrets").join("secrets").exists());
        assert!(!home.path().join("keys").exists());
        assert!(!home.path().join("auth.toml").exists());
        Ok(())
    }

    #[test]
    fn token_file_without_auth_socket_fails_write() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let config = harw_config::ResolvedConfig {
            infrastructure: Some(InfrastructureSection {
                token_file: Some(home.path().join("token")),
                ..InfrastructureSection::default()
            }),
            ..harw_config::ResolvedConfig::default()
        };

        let result = store_secret(
            home.path(),
            &config,
            "provider-openai",
            "provider-auth",
            &SecretString::from("sk-token-file-value".to_owned()),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "token_file without auth_socket must fail the write".into(),
            ));
        };
        assert!(error.contains("token_file requires auth_socket"));
        assert!(!home.path().join("keys").exists());
        assert!(!home.path().join("auth.toml").exists());
        assert!(!home.path().join("sealed-secrets").exists());
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn reader_attaches_wrapper_when_hub_configured() -> TestResult {
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
        config.auth.kek = Some(key_file_kek(&key_path));
        config.infrastructure = Some(InfrastructureSection {
            auth_socket: Some(home.path().join("missing.sock")),
            ..InfrastructureSection::default()
        });

        let resolver = open_configured_secret_resolver(home.path(), &config)
            .map_err(ctx("the reader opens without dialling the hub"))?
            .ok_or(TestError::Missing("sealed provider is configured"))?;
        assert!(resolver.store.dek_wrapper().is_some());
        let resolved = resolver
            .resolve("secrets:provider-token")
            .map_err(ctx("a V2 record still resolves"))?;
        assert_eq!(resolved.expose_secret(), "utf8-token");
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn compat_read_side_unchanged() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let key_path = write_test_kek(home.path())?;
        {
            let mut store = store_with_test_kek(home.path(), &key_path)?;
            store
                .create(
                    "provider-token",
                    "provider authentication",
                    &SecretBox::new(b"sealed-token".to_vec().into_boxed_slice()),
                )
                .map_err(ctx("seal test token"))?;
        }
        let secrets_dir = home.path().join("secrets");
        fs::create_dir(&secrets_dir).map_err(ctx("create plaintext secrets directory"))?;
        let key_file = secrets_dir.join("x.key");
        fs::write(&key_file, b"file-token\n").map_err(ctx("write plaintext key file"))?;
        set_private_permissions(&key_file)?;
        let json_file = secrets_dir.join("x.json");
        fs::write(&json_file, br#"{"k":"json-token"}"#).map_err(ctx("write JSON key file"))?;
        set_private_permissions(&json_file)?;

        let env_var = "HARW_TEST_SECRET_STORE_COMPAT_ENV_7F3A";
        let mut config = sealed_provider_config()?;
        config.auth.kek = Some(key_file_kek(&key_path));
        config
            .env_layer
            .insert(env_var.to_owned(), "env-token".to_owned());
        let resolver = open_configured_secret_resolver(home.path(), &config)
            .map_err(ctx("the configured KEK opens the store"))?
            .ok_or(TestError::Missing("sealed provider is configured"))?;

        let plaintext_refs = [
            (format!("file:{}", key_file.display()), "file-token"),
            (
                format!("file-json:{}#/k", json_file.display()),
                "json-token",
            ),
            (format!("env:{env_var}"), "env-token"),
        ];
        for (reference, expected) in plaintext_refs {
            let provider = provider_with_auth(&reference)?;
            let without = harw_provider_http::resolve_provider_credential(
                &provider,
                &config.env_layer,
                Some(home.path()),
                None,
            )
            .map_err(ctx("plaintext reference resolves without the store"))?
            .ok_or(TestError::Missing("provider has auth"))?;
            let with = harw_provider_http::resolve_provider_credential(
                &provider,
                &config.env_layer,
                Some(home.path()),
                Some(&resolver as &dyn SecretResolver),
            )
            .map_err(ctx("plaintext reference resolves next to the store"))?
            .ok_or(TestError::Missing("provider has auth"))?;
            assert_eq!(without.expose_secret(), expected);
            assert_eq!(with.expose_secret(), expected);
        }

        let sealed = provider_with_auth("secrets:provider-token")?;
        let resolved = harw_provider_http::resolve_provider_credential(
            &sealed,
            &config.env_layer,
            Some(home.path()),
            Some(&resolver as &dyn SecretResolver),
        )
        .map_err(ctx("a V2 secrets: reference resolves"))?
        .ok_or(TestError::Missing("provider has auth"))?;
        assert_eq!(resolved.expose_secret(), "sealed-token");
        Ok(())
    }

    #[test]
    fn credential_pool_secrets_entry_requires_kek() -> TestResult {
        let home = TempDir::new().map_err(ctx("temporary home"))?;
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "pooled".to_owned(),
            provider_with_auth("env:HARW_TEST_POOLED_PROVIDER_TOKEN")?,
        );
        config.auth.credential_pool.insert(
            "pooled".to_owned(),
            vec![CredentialEntry {
                secret: SecretRef::Secrets("pool-token".to_owned()),
                label: None,
                priority: 0,
                base_url: None,
            }],
        );

        let result = open_configured_secret_resolver(home.path(), &config);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a secrets: pool entry without KEK must fail closed".into(),
            ));
        };
        assert!(error.contains("requires a configured KEK"));
        Ok(())
    }

    #[test]
    fn secrets_kek_namespace_matches_purpose() -> TestResult {
        assert_eq!(
            harw_dod_encrypt::HarwKeyPurpose::SecretsKek.namespace(),
            SECRETS_KEK_NAMESPACE
        );
        Ok(())
    }

    fn provider_with_auth(reference: &str) -> TestResult<ProviderToml> {
        toml::from_str::<ProviderToml>(&format!(
            "name = \"compat\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"{reference}\"\n"
        ))
        .map_err(ctx("valid test provider"))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn key_file_kek(key_path: &Path) -> KekConfig {
        KekConfig {
            provenance: ConfigKekProvenance::KeyFile,
            key_file_path: Some(key_path.to_string_lossy().into_owned()),
            keyring_entry: None,
            env_seed_var: None,
        }
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
