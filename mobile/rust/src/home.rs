//! App home: the on-device layout under Android's `filesDir` (the phone has no
//! `$HOME`), the phone profile scaffold and the active model route.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::types::{AppHomeInfo, MobileError};

pub(crate) const PHONE_UIA_ID: &str = "harwness.agent.phone-uia@1";
pub(crate) const PHONE_UIA_DIR: &str = "phone-uia";
pub(crate) const PHONE_OWNER_ID: &str = "phone:owner";

const PHONE_UIA_DEFINITION: &str = include_str!("../assets/agents/phone-uia/definition.toml");
const PHONE_UIA_SYSTEM: &str = include_str!("../assets/agents/phone-uia/system.md");

const CONFIG_HEADER: &str = "# Harw phone profile (managed by the Harw app).\n";
const CONFIG_FILE: &str = "config.toml";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const KEY_UIA: &str = "active_uia_definition";
const KEY_PROVIDER: &str = "default_provider";
const KEY_MODEL: &str = "default_model";
/// Selected provider id of the on-device model (not a route file).
pub(crate) const LOCAL_ROUTE_ID: &str = "local-gguf";
/// File in the state dir that holds the models directory of the on-device model.
const LOCAL_MODELS_DIR_FILE: &str = "local-gguf-dir";
/// File in the state dir that holds the explicit device-tools switch ("on"/"off").
const DEVICE_TOOLS_FILE: &str = "device-tools";

fn internal(detail: impl Into<String>) -> MobileError {
    MobileError::Internal {
        detail: detail.into(),
    }
}

fn io_error(path: &Path, error: &std::io::Error) -> MobileError {
    internal(format!("{}: {error}", path.display()))
}

/// Resolved app layout. Cheap to clone.
#[derive(Clone, Debug)]
pub(crate) struct AppHome {
    files_dir: PathBuf,
    home: PathBuf,
    profile_dir: PathBuf,
    workspace: PathBuf,
    state_dir: PathBuf,
}

impl AppHome {
    /// Resolves the layout under `files_dir` and scaffolds it idempotently.
    /// Never overwrites files the user edited.
    pub(crate) fn open(files_dir: &Path) -> Result<Self, MobileError> {
        if !files_dir.is_absolute() {
            return Err(internal("app files directory must be absolute"));
        }
        fs::create_dir_all(files_dir).map_err(|e| io_error(files_dir, &e))?;
        let home = files_dir.join("harw-home");
        let profile_name = harw_home::active_profile_name(&home);
        let profile_dir =
            harw_home::profile_dir(&home, &profile_name).map_err(|e| internal(e.to_string()))?;
        let app = Self {
            files_dir: files_dir.to_path_buf(),
            workspace: files_dir.join("workspaces").join("default"),
            state_dir: home.join("state").join("local-host"),
            home,
            profile_dir,
        };
        app.scaffold()?;
        Ok(app)
    }

    fn scaffold(&self) -> Result<(), MobileError> {
        create_private_dir(self.home())?;
        create_private_dir(self.profile_dir())?;

        let config = self.config_path();
        if !config.exists() {
            let body = format!(
                "{CONFIG_HEADER}{KEY_UIA} = \"{PHONE_UIA_ID}\"\nsingle_agent_mode = true\n"
            );
            write_private_atomic(&config, body.as_bytes())?;
        }

        harw_home::ensure_home(self.home()).map_err(|e| internal(e.to_string()))?;

        let mut table = self.read_config()?;
        if !table.contains_key(KEY_UIA) {
            table.insert(KEY_UIA.into(), toml::Value::String(PHONE_UIA_ID.into()));
            self.write_config(&table)?;
        }

        let agent_dir = self.profile_dir().join("agents").join(PHONE_UIA_DIR);
        create_private_dir(&agent_dir)?;
        for (name, contents) in [
            ("definition.toml", PHONE_UIA_DEFINITION),
            ("system.md", PHONE_UIA_SYSTEM),
        ] {
            let path = agent_dir.join(name);
            if !path.exists() {
                write_private_atomic(&path, contents.as_bytes())?;
            }
        }

        create_private_dir(self.workspace())?;
        create_private_dir(self.state_dir())?;
        create_private_dir(&self.providers_dir())?;
        Ok(())
    }

    pub(crate) fn files_dir(&self) -> &Path {
        &self.files_dir
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub(crate) fn providers_dir(&self) -> PathBuf {
        self.profile_dir.join("providers")
    }

    fn config_path(&self) -> PathBuf {
        self.profile_dir.join(CONFIG_FILE)
    }

    fn read_config(&self) -> Result<toml::Table, MobileError> {
        let path = self.config_path();
        let file = fs::File::open(&path).map_err(|e| io_error(&path, &e))?;
        let mut text = String::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|e| io_error(&path, &e))?;
        if text.len() as u64 > MAX_CONFIG_BYTES {
            return Err(internal(format!(
                "{}: profile config is larger than 64 KiB",
                path.display()
            )));
        }
        toml::from_str::<toml::Table>(&text).map_err(|_| {
            internal(format!(
                "{}: profile config is not valid TOML",
                path.display()
            ))
        })
    }

    fn write_config(&self, table: &toml::Table) -> Result<(), MobileError> {
        let body = toml::to_string(table)
            .map_err(|_| internal("profile config could not be serialised"))?;
        let text = format!("{CONFIG_HEADER}{body}");
        write_private_atomic(&self.config_path(), text.as_bytes())
    }

    pub(crate) fn info(&self) -> Result<AppHomeInfo, MobileError> {
        let table = self.read_config()?;
        let active_uia = table
            .get(KEY_UIA)
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        let route = self.active_route()?;
        let (active_provider, active_model) = match route {
            Some((provider, model)) => (Some(provider), (!model.is_empty()).then_some(model)),
            None => (None, None),
        };
        Ok(AppHomeInfo {
            files_dir: self.files_dir().to_string_lossy().into_owned(),
            home: self.home().to_string_lossy().into_owned(),
            profile_dir: self.profile_dir().to_string_lossy().into_owned(),
            workspace: self.workspace().to_string_lossy().into_owned(),
            state_dir: self.state_dir().to_string_lossy().into_owned(),
            active_uia,
            active_provider,
            active_model,
        })
    }

    /// Points the process-wide harw home at this app home. Round 2 calls it
    /// once before the local host starts.
    #[allow(dead_code)] // used by the local runtime in round 2
    pub(crate) fn install_process_home(&self) -> Result<(), MobileError> {
        harw_home::set_home_override(self.home.clone()).map_err(|e| internal(e.to_string()))
    }

    /// `(default_provider, default_model or "")` of the active route.
    pub(crate) fn active_route(&self) -> Result<Option<(String, String)>, MobileError> {
        let table = self.read_config()?;
        let Some(provider) = table.get(KEY_PROVIDER).and_then(toml::Value::as_str) else {
            return Ok(None);
        };
        let model = table
            .get(KEY_MODEL)
            .and_then(toml::Value::as_str)
            .unwrap_or("");
        Ok(Some((provider.to_owned(), model.to_owned())))
    }

    pub(crate) fn set_active_route(&self, provider: &str, model: &str) -> Result<(), MobileError> {
        for (what, value) in [("provider", provider), ("model", model)] {
            if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(internal(format!("invalid active route {what}")));
            }
        }
        let mut table = self.read_config()?;
        table.insert(KEY_PROVIDER.into(), toml::Value::String(provider.into()));
        table.insert(KEY_MODEL.into(), toml::Value::String(model.into()));
        self.write_config(&table)
    }

    /// Makes `id` the active UIA definition of the profile.
    pub(crate) fn set_active_uia(&self, id: &str) -> Result<(), MobileError> {
        if id.is_empty() || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(internal("invalid UIA definition id"));
        }
        let mut table = self.read_config()?;
        table.insert(KEY_UIA.into(), toml::Value::String(id.into()));
        self.write_config(&table)
    }

    /// Remembers where the on-device model files live (absolute path).
    pub(crate) fn set_local_models_dir(&self, dir: &str) -> Result<(), MobileError> {
        write_private_atomic(&self.state_dir().join(LOCAL_MODELS_DIR_FILE), dir.as_bytes())
    }

    /// The models directory saved by [`Self::set_local_models_dir`].
    #[cfg_attr(not(feature = "local-host"), allow(dead_code))]
    pub(crate) fn local_models_dir(&self) -> Option<String> {
        let text = fs::read_to_string(self.state_dir().join(LOCAL_MODELS_DIR_FILE)).ok()?;
        let dir = text.trim();
        (!dir.is_empty()).then(|| dir.to_owned())
    }

    /// Model id when the on-device model is the active provider.
    pub(crate) fn active_local_model(&self) -> Result<Option<String>, MobileError> {
        Ok(self
            .active_route()?
            .filter(|(provider, model)| provider == LOCAL_ROUTE_ID && !model.is_empty())
            .map(|(_, model)| model))
    }

    /// Whether the agent's device tools are mounted: an explicit choice wins, else
    /// off while the on-device model is the active provider and on otherwise.
    pub(crate) fn device_tools_enabled(&self) -> Result<bool, MobileError> {
        let explicit = fs::read_to_string(self.state_dir().join(DEVICE_TOOLS_FILE))
            .ok()
            .and_then(|text| match text.trim() {
                "on" => Some(true),
                "off" => Some(false),
                _ => None,
            });
        match explicit {
            Some(enabled) => Ok(enabled),
            None => Ok(self.active_local_model()?.is_none()),
        }
    }

    /// Sets (`Some`) or clears (`None`) the explicit device-tools choice.
    pub(crate) fn set_device_tools_setting(&self, enabled: Option<bool>) -> Result<(), MobileError> {
        let path = self.state_dir().join(DEVICE_TOOLS_FILE);
        match enabled {
            Some(on) => write_private_atomic(&path, if on { b"on" } else { b"off" }),
            None => match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io_error(&path, &e)),
            },
        }
    }

    pub(crate) fn clear_active_route_if(&self, provider: &str) -> Result<(), MobileError> {
        let mut table = self.read_config()?;
        if table.get(KEY_PROVIDER).and_then(toml::Value::as_str) == Some(provider) {
            table.remove(KEY_PROVIDER);
            table.remove(KEY_MODEL);
            self.write_config(&table)?;
        }
        Ok(())
    }

    #[cfg(feature = "local-host")]
    #[allow(dead_code)] // used by the local runtime in round 2
    pub(crate) fn local_host_config(&self) -> harw_local_host::LocalHostConfig {
        harw_local_host::LocalHostConfig {
            home: self.home.clone(),
            workspace: self.workspace.clone(),
            state_dir: self.state_dir.clone(),
            owner: harw_local_host::LocalOwner::new(
                PHONE_OWNER_ID,
                harw_types::PermissionTier::Owner,
            ),
            rights: harw_local_host::HostRights::operator(),
        }
    }
}

/// Writes `<path>.tmp` with mode 0600, syncs it and renames it over `path`.
pub(crate) fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<(), MobileError> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);

    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|e| io_error(path, &e))?;
    file.write_all(contents).map_err(|e| io_error(path, &e))?;
    file.sync_all().map_err(|e| io_error(path, &e))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| io_error(path, &e))
}

/// `create_dir_all`, then 0700 on unix.
pub(crate) fn create_private_dir(path: &Path) -> Result<(), MobileError> {
    fs::create_dir_all(path).map_err(|e| io_error(path, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| io_error(path, &e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn profile_table(home: &AppHome) -> TestResult<toml::Table> {
        Ok(toml::from_str(&fs::read_to_string(
            home.profile_dir().join(CONFIG_FILE),
        )?)?)
    }

    #[test]
    fn layout_and_dirs_exist() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        assert_eq!(home.home(), dir.path().join("harw-home"));
        assert_eq!(home.workspace(), dir.path().join("workspaces/default"));
        assert_eq!(
            home.state_dir(),
            dir.path().join("harw-home/state/local-host")
        );
        assert!(home.profile_dir().starts_with(home.home().join("profiles")));
        for path in [
            home.home(),
            home.profile_dir(),
            home.workspace(),
            home.state_dir(),
            home.providers_dir().as_path(),
        ] {
            assert!(path.is_dir(), "{} missing", path.display());
        }
        let info = home.info()?;
        assert_eq!(info.active_uia.as_deref(), Some(PHONE_UIA_ID));
        assert_eq!(info.active_provider, None);
        Ok(())
    }

    #[test]
    fn profile_config_selects_the_phone_uia() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        let table = profile_table(&home)?;
        assert_eq!(
            table.get(KEY_UIA).and_then(toml::Value::as_str),
            Some(PHONE_UIA_ID)
        );
        assert_eq!(
            table
                .get("single_agent_mode")
                .and_then(toml::Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn second_open_keeps_user_edits() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        let system = home
            .profile_dir()
            .join("agents")
            .join(PHONE_UIA_DIR)
            .join("system.md");
        fs::write(&system, "my own prompt")?;
        AppHome::open(dir.path())?;
        assert_eq!(fs::read_to_string(&system)?, "my own prompt");
        Ok(())
    }

    #[test]
    fn existing_config_gets_the_uia_key_and_keeps_other_keys() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home_dir = dir.path().join("harw-home");
        let profile =
            harw_home::profile_dir(&home_dir, &harw_home::active_profile_name(&home_dir))?;
        fs::create_dir_all(&profile)?;
        fs::write(profile.join(CONFIG_FILE), "custom_key = \"keep\"\n")?;
        let home = AppHome::open(dir.path())?;
        let table = profile_table(&home)?;
        assert_eq!(
            table.get("custom_key").and_then(toml::Value::as_str),
            Some("keep")
        );
        assert_eq!(
            table.get(KEY_UIA).and_then(toml::Value::as_str),
            Some(PHONE_UIA_ID)
        );
        Ok(())
    }

    #[test]
    fn active_route_set_and_clear() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        home.set_active_route("route-a", "model-x")?;
        assert_eq!(
            home.active_route()?,
            Some(("route-a".to_owned(), "model-x".to_owned()))
        );
        home.clear_active_route_if("other")?;
        assert!(home.active_route()?.is_some());
        home.clear_active_route_if("route-a")?;
        assert_eq!(home.active_route()?, None);
        assert_eq!(
            profile_table(&home)?
                .get(KEY_UIA)
                .and_then(toml::Value::as_str),
            Some(PHONE_UIA_ID)
        );
        assert!(home.set_active_route("bad id", "m").is_err());
        assert!(home.set_active_route("a", "").is_err());
        Ok(())
    }

    #[test]
    fn asset_definition_parses() -> TestResult {
        let table: toml::Table = toml::from_str(PHONE_UIA_DEFINITION)?;
        assert_eq!(
            table.get("schema").and_then(toml::Value::as_str),
            Some("harwness.agent/v1")
        );
        assert_eq!(
            table.get("id").and_then(toml::Value::as_str),
            Some(PHONE_UIA_ID)
        );
        assert_eq!(
            table.get("role").and_then(toml::Value::as_str),
            Some("user-interface")
        );
        Ok(())
    }

    #[test]
    fn relative_files_dir_is_rejected() {
        assert!(AppHome::open(Path::new("relative/files")).is_err());
    }

    #[cfg(feature = "local-host")]
    #[tokio::test]
    async fn phone_uia_lowers_in_the_real_runtime() -> TestResult {
        use harw_local_host::{CoreSessionFactory, SessionWiring};
        use std::sync::Arc;

        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        let factory = home
            .local_host_config()
            .core_factory(Arc::new(harw_core::EchoModelProvider::new("hi")));
        let (event_tx, _e) = tokio::sync::mpsc::unbounded_channel();
        let (turn_tx, _t) = tokio::sync::mpsc::unbounded_channel();
        let core = factory.build(
            &harw_types::SessionId::new(),
            None,
            SessionWiring { event_tx, turn_tx },
        )?;
        let actor = core
            .session
            .spawn_context()
            .and_then(|c| c.approval_actor.clone());
        assert_eq!(
            actor,
            Some(harw_types::ApprovalActor::Operator {
                id: PHONE_OWNER_ID.into()
            })
        );
        Ok(())
    }
}
