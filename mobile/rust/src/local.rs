//! Opens the in-process (on-device) harw session host as a [`Backend`].

use std::{path::PathBuf, sync::Arc};

use crate::{MobileError, PlatformKeystore, backend::Backend};

/// Client label announced to the on-device host in the hello.
#[cfg_attr(not(feature = "local-host"), allow(dead_code))]
pub(crate) const LOCAL_CLIENT_LABEL: &str = "harw-android-local";

/// Error detail of builds without the on-device host.
pub(crate) const LOCAL_HOST_UNAVAILABLE: &str = "the on-device host is not available in this build";

#[cfg(feature = "local-host")]
static LOCAL_INFERENCE: std::sync::LazyLock<harw_llama::LocalInferenceControl> =
    std::sync::LazyLock::new(|| {
        let control = harw_llama::LocalInferenceControl::default();
        let _ = control.set_image_concurrency(
            crate::local_vision::DEFAULT_LOCAL_VISION_CONCURRENCY as u8,
        );
        control
    });

/// Model loads (including host reopen) cannot overtake a local vision batch
/// while its worker or cancellation cleanup still owns the device model.
#[cfg(feature = "local-host")]
static LOCAL_MODEL_LIFECYCLE: std::sync::LazyLock<Arc<tokio::sync::Mutex<()>>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(())));

#[cfg(feature = "local-host")]
async fn load_model<R: Send + 'static>(
    load: impl FnOnce() -> R + Send + 'static,
) -> Result<R, tokio::task::JoinError> {
    let fence = Arc::clone(&LOCAL_MODEL_LIFECYCLE).lock_owned().await;
    tokio::task::spawn_blocking(move || {
        let _fence = fence;
        load()
    })
    .await
}

/// Change the on-device cap for new inference calls without reloading a model.
/// Invalid values or builds without a local host return false.
#[uniffi::export]
pub fn set_local_image_concurrency(count: u8) -> bool {
    #[cfg(feature = "local-host")]
    {
        LOCAL_INFERENCE.set_image_concurrency(count)
    }
    #[cfg(not(feature = "local-host"))]
    {
        let _ = count;
        false
    }
}

/// Builds without the `local-host` feature have no on-device host.
#[cfg(not(feature = "local-host"))]
pub(crate) async fn open_local(
    files_dir: PathBuf,
    keystore: Arc<dyn PlatformKeystore>,
    features: Vec<String>,
    device: crate::device_tools::DeviceHandles,
) -> Result<Backend, MobileError> {
    let _ = (files_dir, keystore, features, device);
    Err(MobileError::Session {
        detail: LOCAL_HOST_UNAVAILABLE.into(),
    })
}

#[cfg(feature = "local-host")]
fn host_task_failed() -> MobileError {
    MobileError::Internal {
        detail: "on-device host task failed".into(),
    }
}

/// Opens the app home, builds the model of the active route and starts the host.
#[cfg(feature = "local-host")]
pub(crate) async fn open_local(
    files_dir: PathBuf,
    keystore: Arc<dyn PlatformKeystore>,
    features: Vec<String>,
    device: crate::device_tools::DeviceHandles,
) -> Result<Backend, MobileError> {
    use crate::{home::AppHome, providers::ProviderStore};

    let home = AppHome::open(&files_dir)?;
    home.install_process_home()?;
    let store = ProviderStore::from_parts(home.clone(), keystore);
    let resolver = store.resolver().clone();
    let model_home = home.clone();
    let selection = home.active_route()?;
    let fingerprint = projector_fingerprint(&home, selection.as_ref());
    let revision = provider_revision(&home, selection.as_ref());
    let resolver_for_build = resolver.clone();
    let selected = selection.clone();
    let built = load_model(move || build_selected_model(&model_home, &resolver_for_build, selected.as_ref()))
        .await
        .map_err(|_| host_task_failed())??;
    // The host keeps this wrapper for its whole life: a later provider/model
    // activation is picked up by the next turn (see `SwitchingModel`).
    let model = Arc::new(SwitchingModel {
        home: home.clone(),
        baseline_persona: crate::uia::effective_session_persona(&home, None),
        resolver,
        current: tokio::sync::Mutex::new((
            ModelCacheKey::Route(selection),
            fingerprint,
            revision,
            built,
        )),
    });
    // The device tools follow the profile switch (off by default for the on-device model).
    let (shell, files) = device;
    let mut contributors: Vec<Arc<dyn harw_local_host::ext::AssemblyContributor>> = Vec::new();
    if (shell.is_some() || files.is_some()) && home.device_tools_enabled()? {
        contributors.push(Arc::new(crate::device_tools::DeviceToolsContributor::new(
            shell, files,
        )));
    }
    // A phone model has a small context window: the on-device GGUF is
    // text-only ("nur Text, keine Werkzeuge"), a cloud route sees a lean set.
    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    remember_uia_runtime(&model);
    let surface = if home.active_local_model()?.is_some() {
        harw_local_host::ToolSurface::default()
    } else {
        harw_local_host::ToolSurface {
            allow: LEAN_TOOLS.iter().map(|name| (*name).to_owned()).collect(),
        }
    };
    let host_provider: Arc<dyn harw_core::ModelProvider> = model.clone();
    let mut backend =
        open_local_with_tools(home, host_provider, features, contributors, Some(surface)).await?;
    backend.hybrid_model = Some(model);
    Ok(backend)
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
static UIA_RUNTIME: std::sync::Mutex<Option<std::sync::Weak<SwitchingModel>>> =
    std::sync::Mutex::new(None);

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
fn remember_uia_runtime(model: &Arc<SwitchingModel>) {
    if let Ok(mut slot) = UIA_RUNTIME.lock() { *slot = Some(Arc::downgrade(model)); }
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) fn uia_runtime_for(files: &std::path::Path) -> Option<Arc<SwitchingModel>> {
    let model = UIA_RUNTIME.lock().ok()?.as_ref()?.upgrade()?;
    matches_uia_home(&model.home, files).then_some(model)
}

#[cfg(all(feature = "vault-uia-chat", any(target_os = "android", test)))]
fn matches_uia_home(home: &crate::home::AppHome, files: &std::path::Path) -> bool {
    if home.files_dir() != files {
        return false;
    }
    let selection = harw_home::active_profile_name(home.home());
    harw_home::profile_dir(home.home(), &selection).ok().as_deref() == Some(home.profile_dir())
}

/// Model provider of the on-device host that follows the active selection.
///
/// The host builds its model once; without this wrapper a provider activated
/// later (another route, or the on-device GGUF) would not reach the running
/// host. Before every turn the wrapper compares the active route and model of
/// the app home with the one it was built for and rebuilds on a change.
/// Ordinary chat bindings are resolved from the profile's session assignments;
/// the request's model IDs are cleared only after choosing the concrete route.
#[cfg(feature = "local-host")]
pub(crate) struct SwitchingModel {
    home: crate::home::AppHome,
    baseline_persona: Option<String>,
    resolver: crate::secrets::KeystoreSecretResolver,
    current: tokio::sync::Mutex<ProviderCache>,
}

#[cfg(feature = "local-host")]
type ProviderCache = (ModelCacheKey, Option<ProjectorFingerprint>, Option<blake3::Hash>, CachedProvider);

#[cfg(feature = "local-host")]
#[derive(PartialEq, Eq)]
enum ModelCacheKey {
    Route(Option<(String, String)>),
    LocalAssistant(String, VisionFileFingerprint),
    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    PrivateAssistantVision(String, VisionFiles),
}

#[cfg(feature = "local-host")]
#[derive(Clone)]
enum CachedProvider {
    Other(Arc<dyn harw_core::ModelProvider>),
    Local {
        model: Arc<harw_llama::LocalGgufProvider>,
        vision_files: Option<VisionFiles>,
        loaded_revision: u64,
    },
}

#[cfg(feature = "local-host")]
impl CachedProvider {
    fn for_chat(&self) -> Arc<dyn harw_core::ModelProvider> {
        match self {
            Self::Other(model) => Arc::clone(model),
            Self::Local { model, .. } => model.clone(),
        }
    }
}

#[cfg(feature = "local-host")]
#[derive(Clone, PartialEq, Eq)]
struct VisionFiles {
    model: VisionFileFingerprint,
    projector: VisionFileFingerprint,
}

/// A concrete non-lean private GGUF/projector pair, prepared while its
/// caller is foreground-authorized. Owns only the exact local engine/path
/// identity and the remapping fence; no host, route resolver or vault.
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) struct PinnedPrivateAssistantVision {
    selected: (String, VisionFiles),
    explicit_model: bool,
    home: crate::home::AppHome,
    model: Arc<harw_llama::LocalGgufProvider>,
    _mapping: tokio::sync::OwnedMutexGuard<()>,
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
impl PinnedPrivateAssistantVision {
    pub(crate) fn is_current(&self) -> bool {
        private_assistant_vision_selection(
            &self.home, self.explicit_model.then_some(self.selected.0.as_str()),
        ).as_ref() == Some(&self.selected)
    }

    pub(crate) async fn describe_library_image(
        self,
        image: Arc<[u8]>,
        cancel: &harw_types::cancel::CancelToken,
        on_result: &(dyn Fn(harw_llama::LocalImageDescription) + Send + Sync),
    ) -> Result<(), harw_core::ModelError> {
        if !self.is_current() || cancel.is_cancelled() {
            return Err(harw_core::ModelError::Cancelled);
        }
        let result = self.model.describe_selected_images(vec![(1, image)], cancel, on_result).await;
        LOCAL_INFERENCE.wait_idle().await;
        if !self.is_current() || cancel.is_cancelled() {
            return Err(harw_core::ModelError::Cancelled);
        }
        result
    }

    /// Consume the pin. Dispatch cannot load/reselect an engine or fall back
    /// to HTTP or captions. Await native completion before dropping the fence.
    pub(crate) async fn respond(
        self, request: harw_core::ModelRequest, jpeg: impl AsRef<[u8]> + Send + 'static,
        first_output: impl FnOnce() + Send + 'static,
    ) -> Result<harw_core::ModelResponse, crate::uia_room::PrivateVisionError> {
        use crate::uia_room::PrivateVisionError::{Cancelled, Incomplete, Inference, Model};
        let cancel = request.cancel.clone().ok_or(Cancelled)?;
        if cancel.is_cancelled() { return Err(Cancelled); }
        if !self.is_current() { return Err(Model); }
        let result = self.model.respond_with_private_image_observed(request, jpeg, first_output).await;
        if cancel.is_cancelled() { return Err(Cancelled); }
        if !self.is_current() { return Err(Model); }
        result.map_err(|error| match error {
            harw_core::ModelError::Cancelled => Cancelled,
            harw_core::ModelError::ContextLength { .. } => Incomplete,
            _ => Inference,
        })
    }
}

/// Exact files of the active local profile and explicitly selected GGUF.
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
fn private_assistant_vision_selection(
    home: &crate::home::AppHome, model: Option<&str>,
) -> Option<(String, VisionFiles)> {
    let active = harw_home::active_profile_name(home.home());
    if harw_home::profile_dir(home.home(), &active).ok().as_deref() != Some(home.profile_dir()) {
        return None;
    }
    let id = match model {
        Some(id) => id.to_owned(),
        None => home.active_local_model().ok()??,
    };
    let dir = home.local_models_dir()?;
    let projector = crate::models::vision_path_in(std::path::Path::new(&dir), &id)?;
    let model = crate::models::model_path(dir, id.clone()).ok()?;
    Some((id, vision_files(&model, Some(&projector))?))
}

#[cfg(feature = "local-host")]
#[derive(Clone, PartialEq, Eq)]
struct VisionFileFingerprint {
    path: String,
    length: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    file_identity: (u64, u64, i64, i64),
}

#[cfg(feature = "local-host")]
fn vision_file_fingerprint(path: &str) -> Option<VisionFileFingerprint> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;

    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(VisionFileFingerprint {
        path: path.to_owned(),
        length: meta.len(),
        modified: meta.modified().ok()?,
        #[cfg(unix)]
        file_identity: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
    })
}

#[cfg(feature = "local-host")]
fn vision_files(path: &str, projector: Option<&str>) -> Option<VisionFiles> {
    Some(VisionFiles {
        model: vision_file_fingerprint(path)?,
        projector: vision_file_fingerprint(projector?)?,
    })
}

#[cfg(feature = "local-host")]
static MODEL_REVISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Path, size and modification time of the projector of the active on-device
/// model; a changed value means the model must be reloaded (the add-on was
/// downloaded or replaced after the model was loaded without it).
#[cfg(feature = "local-host")]
type ProjectorFingerprint = (String, u64, Option<std::time::SystemTime>);

#[cfg(feature = "local-host")]
fn projector_fingerprint(
    home: &crate::home::AppHome,
    selection: Option<&(String, String)>,
) -> Option<ProjectorFingerprint> {
    let (provider, model_id) = selection?;
    if provider != crate::home::LOCAL_ROUTE_ID {
        return None;
    }
    let dir = home.local_models_dir()?;
    let path = crate::models::vision_path_in(std::path::Path::new(&dir), model_id)?;
    let meta = std::fs::metadata(&path).ok()?;
    Some((path, meta.len(), meta.modified().ok()))
}

/// The active provider's app-managed route file is atomically replaced on edit.
#[cfg(feature = "local-host")]
fn provider_revision(
    home: &crate::home::AppHome,
    selection: Option<&(String, String)>,
) -> Option<blake3::Hash> {
    use std::io::Read;
    let (provider, _) = selection?;
    if provider == crate::home::LOCAL_ROUTE_ID
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    let path = home.providers_dir().join(format!("{provider}.toml"));
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= 64 * 1024).then(|| blake3::hash(&bytes))
}

#[cfg(feature = "local-host")]
impl SwitchingModel {
    pub(crate) fn home(&self) -> &crate::home::AppHome {
        &self.home
    }

    pub(crate) fn resolver(&self) -> &crate::secrets::KeystoreSecretResolver {
        &self.resolver
    }

    pub(crate) fn hybrid_qwen_id(&self) -> Result<Option<String>, MobileError> {
        if !harw_llama::engine_available() {
            return Ok(None);
        }
        Ok(self
            .home
            .active_local_model()?
            .filter(|id| id.starts_with("qwen3.5-") || id.starts_with("qwen3-")))
    }

    pub(crate) async fn pin_hybrid_qwen(
        &self,
        selected: &str,
    ) -> Result<Arc<dyn harw_core::ModelProvider>, MobileError> {
        let changed = || MobileError::Session {
            detail: "Das gewählte lokale Qwen ist nicht aktiv".into(),
        };
        if self.hybrid_qwen_id()?.as_deref() != Some(selected) {
            return Err(changed());
        }
        let provider = self.provider().await.map_err(|_| changed())?;
        if provider.pinned_provider_id().as_deref() != Some(harw_llama::LOCAL_PROVIDER_ID)
            || provider.pinned_model_id().as_deref() != Some(selected)
        {
            return Err(changed());
        }
        Ok(provider)
    }

    /// Pin the locally selected GGUF without constructing or falling back to
    /// an HTTP provider, even if the profile changes while loading.
    pub(crate) async fn pin_local_assistant_model(
        &self, selected: Option<&str>,
    ) -> Result<Arc<dyn harw_core::ModelProvider>, MobileError> {
        let missing = || MobileError::Session {
            detail: "Wähle zuerst ein lokales GGUF-Modell".into(),
        };
        let selected_file = || -> Result<(String, VisionFileFingerprint), MobileError> {
            let id = match selected {
                Some(id) => id.to_owned(),
                None => self.home.active_local_model()?.ok_or_else(missing)?,
            };
            let dir = self.home.local_models_dir().ok_or_else(missing)?;
            let path = crate::models::model_path(dir, id.clone()).map_err(|_| missing())?;
            let file = vision_file_fingerprint(&path).ok_or_else(missing)?;
            Ok((id, file))
        };
        let (id, file) = selected_file()?;
        // The UIA text-only provider is distinct from the coding/vision
        // profile even when it maps the same GGUF.
        let wanted = ModelCacheKey::LocalAssistant(id.clone(), file.clone());
        let mut current = self.current.lock().await;
        if selected_file()? != (id.clone(), file.clone()) {
            return Err(missing());
        }
        if current.0 != wanted || !crate::uia_room::is_pinned_local(&current.3.for_chat(), &id) {
            // Release the old model before mapping another; share the same
            // cache lock as the coding host.
            current.3 = CachedProvider::Other(Arc::new(NoModel));
            let home = self.home.clone();
            let selected = id.clone();
            let built = load_model(move || build_local_assistant_gguf(&home, &selected))
                .await
                .map_err(|_| missing())??;
            *current = (wanted, None, None, built);
        }
        if selected_file()? != (id.clone(), file)
            || !crate::uia_room::is_pinned_local(&current.3.for_chat(), &id)
        {
            return Err(missing());
        }
        Ok(current.3.for_chat())
    }

    /// Resolve and load only the exact local model/projector. The caller's
    /// native authorization is still live here; model admission is complete
    /// before a durable inference attempt can be created.
    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    pub(crate) async fn pin_private_assistant_vision(
        &self, cancel: &harw_core::cancel::CancelToken, model_id: Option<&str>,
    ) -> Result<PinnedPrivateAssistantVision, crate::uia_room::PrivateVisionError> {
        use crate::uia_room::PrivateVisionError::{Busy, Cancelled, Model};
        use harw_core::ModelProvider as _;

        let selected = private_assistant_vision_selection(&self.home, model_id).ok_or(Model)?;
        let wanted = ModelCacheKey::PrivateAssistantVision(selected.0.clone(), selected.1.clone());
        let mut current = self.current.try_lock().map_err(|_| Busy)?;
        let mut mapping = Arc::clone(&LOCAL_MODEL_LIFECYCLE).try_lock_owned().map_err(|_| Busy)?;
        if cancel.is_cancelled() { return Err(Cancelled); }
        let cached_exact = current.0 == wanted && matches!(
            &current.3,
            CachedProvider::Local { model, vision_files: Some(files), .. }
                if files == &selected.1 && model.supports_vision()
        );
        if !cached_exact {
            // Fence out another load or vision worker until this exact native
            // operation finishes. Don't retain two GGUF mappings in the cache.
            LOCAL_INFERENCE.wait_idle().await;
            if cancel.is_cancelled() { return Err(Cancelled); }
            current.3 = CachedProvider::Other(Arc::new(NoModel));
            let home = self.home.clone();
            let id = selected.0.clone();
            // Keep remapping exclusion with the blocking loader even if
            // the preparing future is dropped while native GGUF opens.
            let (built, held_mapping) = tokio::task::spawn_blocking(move || {
                let result = build_local_gguf_profile(&home, &id, false, true);
                (result, mapping)
            }).await.map_err(|_| Model)?;
            mapping = held_mapping;
            let built = built.map_err(|_| Model)?;
            // Do not cache failed/changed projector loads as a valid pin.
            if !matches!(&built, CachedProvider::Local { vision_files: Some(files), .. } if files == &selected.1) {
                return Err(Model);
            }
            *current = (wanted, None, None, built);
        }
        if cancel.is_cancelled() { return Err(Cancelled); }
        if private_assistant_vision_selection(&self.home, model_id).as_ref() != Some(&selected) {
            return Err(Model);
        }
        let CachedProvider::Local { model, vision_files: Some(files), .. } = &current.3 else {
            return Err(Model);
        };
        if files != &selected.1 || model.pinned_model_id().as_deref() != Some(selected.0.as_str())
            || model.pinned_provider_id().as_deref() != Some(harw_llama::LOCAL_PROVIDER_ID)
            || !model.supports_vision()
        {
            return Err(Model);
        }
        let model = Arc::clone(model);
        drop(current);
        Ok(PinnedPrivateAssistantVision {
            selected, explicit_model: model_id.is_some(), home: self.home.clone(), model, _mapping: mapping,
        })
    }

    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    pub(crate) fn private_library_vision_models(&self) -> Result<Vec<String>, MobileError> {
        let Some(dir) = self.home.local_models_dir() else {
            return Ok(Vec::new());
        };
        Ok(crate::models::local_models(dir.clone())?.into_iter()
            .filter_map(|model| {
                private_assistant_vision_selection(&self.home, Some(&model.id)).map(|(id, _)| id)
            })
            .collect())
    }

    pub(crate) fn session_title_local_choices(&self) -> Result<Vec<crate::SessionTitleLocalChoice>, MobileError> {
        let current = self.current.try_lock().map_err(|_| local_vision_busy())?;
        Ok(self.loaded_title_model(&current.0, &current.3)?
            .map(|(_, choice)| choice).into_iter().collect())
    }

    fn loaded_title_model<'a>(
        &self,
        key: &ModelCacheKey,
        cached: &'a CachedProvider,
    ) -> Result<Option<(&'a harw_llama::LocalGgufProvider, crate::SessionTitleLocalChoice)>, MobileError> {
        use harw_core::ModelProvider as _;
        let active = harw_home::active_profile_name(self.home.home());
        if harw_home::profile_dir(self.home.home(), &active).ok().as_deref() != Some(self.home.profile_dir()) {
            return Ok(None);
        }
        let Some(id) = self.home.active_local_model()? else { return Ok(None); };
        let matches_key = match key {
            ModelCacheKey::Route(Some((provider, model))) => provider == crate::home::LOCAL_ROUTE_ID && model == &id,
            ModelCacheKey::LocalAssistant(model, _) => model == &id,
            _ => false,
        };
        let CachedProvider::Local { model, loaded_revision, .. } = cached else { return Ok(None); };
        if !matches_key || model.pinned_provider_id().as_deref() != Some(harw_llama::LOCAL_PROVIDER_ID) ||
            model.pinned_model_id().as_deref() != Some(id.as_str())
        {
            return Ok(None);
        }
        Ok(Some((model.as_ref(), crate::SessionTitleLocalChoice {
            model_id: id,
            loaded_revision: *loaded_revision,
        })))
    }

    pub(crate) async fn generate_pinned_local_title(
        &self,
        choice: &crate::SessionTitleLocalChoice,
        prompt: String,
        cancel: &harw_types::cancel::CancelToken,
    ) -> Result<String, MobileError> {
        use harw_core::ModelProvider as _;
        let current = self.current.try_lock().map_err(|_| local_vision_busy())?;
        let _lifecycle = Arc::clone(&LOCAL_MODEL_LIFECYCLE).try_lock_owned()
            .map_err(|_| local_vision_busy())?;
        let (model, _) = self.loaded_title_model(&current.0, &current.3)?
            .filter(|(_, loaded)| loaded == choice)
            .ok_or_else(|| MobileError::Session { detail: "Das gewählte lokale Titelmodell ist nicht mehr geladen.".into() })?;
        if cancel.is_cancelled() { return Err(MobileError::Disconnected); }
        let request = crate::session_title::local_request(prompt, cancel.clone());
        let result = model.respond(request).await;
        LOCAL_INFERENCE.wait_idle().await;
        let output = result.map_err(|_| MobileError::Session { detail: "Das lokale Modell hat keinen Titel geliefert.".into() })?;
        if cancel.is_cancelled() { return Err(MobileError::Disconnected); }
        crate::session_title::complete_response(output)
    }

    pub(crate) fn local_vision_choices(
        &self,
    ) -> Result<Vec<crate::LocalVisionChoice>, MobileError> {
        let current = self.current.try_lock().map_err(|_| local_vision_busy())?;
        let Some(model_id) = self.home.active_local_model()? else {
            return Ok(Vec::new());
        };
        Ok(self
            .exact_loaded_vision(&model_id, &current.0, &current.3)
            .map(|(model, loaded_revision)| crate::LocalVisionChoice {
                model_id,
                effective_parallelism: model.effective_image_concurrency() as u8,
                loaded_revision,
            })
            .into_iter()
            .collect())
    }

    fn exact_loaded_vision<'a>(
        &self,
        selected: &str,
        key: &ModelCacheKey,
        entry: &'a CachedProvider,
    ) -> Option<(&'a harw_llama::LocalGgufProvider, u64)> {
        use harw_core::ModelProvider as _;

        let CachedProvider::Local {
            model,
            vision_files: Some(loaded_files),
            loaded_revision,
        } = entry
        else {
            return None;
        };
        let matches_selection = match key {
            ModelCacheKey::Route(Some((provider, id))) => {
                provider == crate::home::LOCAL_ROUTE_ID && id == selected
            }
            #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
            ModelCacheKey::PrivateAssistantVision(id, files) => id == selected && files == loaded_files,
            _ => false,
        };
        if !matches_selection
            || self.home.active_local_model().ok()?.as_deref() != Some(selected)
            || model.pinned_provider_id().as_deref() != Some(harw_llama::LOCAL_PROVIDER_ID)
            || model.pinned_model_id()?.as_str() != selected
            || !model.supports_vision()
        {
            return None;
        }
        let dir = self.home.local_models_dir()?;
        let mmproj = crate::models::vision_path_in(std::path::Path::new(&dir), selected)?;
        let gguf = crate::models::model_path(dir, selected.to_owned()).ok()?;
        (vision_files(&gguf, Some(&mmproj)).as_ref() == Some(loaded_files))
            .then_some((model.as_ref(), *loaded_revision))
    }

    /// Acquire cache and process-wide load exclusion without ever activating or
    /// mapping another model. Even a disconnected or cancelled FFI caller keeps
    /// its supervisor working through the last blocking native worker.
    pub(crate) async fn describe_pinned_local_vision(
        &self,
        choice: &crate::LocalVisionChoice,
        images: Vec<(u64, Arc<[u8]>)>,
        cancel: &harw_types::cancel::CancelToken,
        on_result: &(dyn Fn(harw_llama::LocalImageDescription) + Send + Sync),
    ) -> Result<(), MobileError> {
        let current = self.current.try_lock().map_err(|_| local_vision_busy())?;
        let _lifecycle = Arc::clone(&LOCAL_MODEL_LIFECYCLE)
            .try_lock_owned()
            .map_err(|_| local_vision_busy())?;
        let (model, _) = self.exact_loaded_vision(&choice.model_id, &current.0, &current.3)
            .filter(|(_, revision)| *revision == choice.loaded_revision)
            .ok_or_else(|| MobileError::Session {
            detail: "Das ausgewählte GGUF-Modell oder sein Projektor ist nicht mehr geladen. Bitte Auswahl aktualisieren.".into(),
        })?;
        let result = model
            .describe_selected_images(images, cancel, on_result)
            .await;
        LOCAL_INFERENCE.wait_idle().await;
        match result {
            Ok(()) => Ok(()),
            Err(harw_core::ModelError::Cancelled) => Err(MobileError::Session {
                detail: "Lokale Bildanalyse gestoppt.".into(),
            }),
            Err(_) => Err(MobileError::Session {
                detail: "Lokale Bildanalyse fehlgeschlagen.".into(),
            }),
        }
    }

    async fn provider(&self) -> Result<Arc<dyn harw_core::ModelProvider>, harw_core::ModelError> {
        let selection = self.home.active_route()
            .map_err(|e| harw_core::ModelError::RequestFailed(e.to_string()))?;
        Ok(self.cached_provider_for(selection).await?.3.for_chat())
    }

    async fn cached_provider_for(
        &self,
        selection: Option<(String, String)>,
    ) -> Result<tokio::sync::MutexGuard<'_, ProviderCache>, harw_core::ModelError> {
        let failed = |detail: String| harw_core::ModelError::RequestFailed(detail);
        let fingerprint = projector_fingerprint(&self.home, selection.as_ref());
        let revision = provider_revision(&self.home, selection.as_ref());
        let wanted = ModelCacheKey::Route(selection.clone());
        let mut current = self.current.lock().await;
        if current.0 != wanted || current.1 != fingerprint || current.2 != revision {
            // No second large GGUF while an existing pin or worker is alive.
            LOCAL_INFERENCE.wait_idle().await;
            if matches!(&current.3, CachedProvider::Local { model, .. } if Arc::strong_count(model) > 1) {
                return Err(failed("Das lokale Modell wird noch verwendet. Bitte erneut versuchen.".into()));
            }
            *current = (ModelCacheKey::Route(None), None, None, CachedProvider::Other(Arc::new(NoModel)));
            let home = self.home.clone();
            let resolver = self.resolver.clone();
            let built = load_model(move || build_selected_model(&home, &resolver, selection.as_ref()))
                .await
                .map_err(|_| failed("on-device model switch failed".to_owned()))?
                .map_err(|e| failed(e.to_string()))?;
            *current = (wanted, fingerprint, revision, built);
        }
        Ok(current)
    }
}

#[cfg(feature = "local-host")]
fn local_vision_busy() -> MobileError {
    MobileError::Busy {
        detail: "Das lokale Modell wird gerade verwendet oder lädt noch.".into(),
    }
}

#[cfg(feature = "local-host")]
impl harw_core::ModelProvider for SwitchingModel {
    fn respond<'a>(&'a self, mut request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        Box::pin(async move {
            let session = harw_core::current_session_id();
            let persona = session.as_ref().and_then(|id| {
                crate::uia::effective_session_persona(&self.home, Some(id.as_str()))
            });
            if let Some(chosen) = persona.as_deref() {
                request.system_prompt = with_session_persona(
                    &request.system_prompt, chosen, self.baseline_persona.as_deref(),
                );
            }
            let selection = resolve_chat_route(&self.home, session.as_ref().map(|id| id.as_str()))
                .map_err(|e| harw_core::ModelError::RequestFailed(e.to_string()))?;
            let current = self.cached_provider_for(selection).await?;
            request.model_id = None;
            request.provider_id = None;
            match &current.3 {
                CachedProvider::Local { model, .. } => {
                    let compact = persona.as_deref().and_then(crate::uia::compact_chat_persona);
                    // Keep the cache locked until the local inference worker
                    // finishes; another chat may not remap the active GGUF.
                    let response = model.respond_with_chat_persona(request, compact).await;
                    LOCAL_INFERENCE.wait_idle().await;
                    response
                }
                CachedProvider::Other(model) => {
                    let model = Arc::clone(model);
                    drop(current);
                    model.respond(request).await
                }
            }
        })
    }
}

/// Keep the host instructions and apply the chat's current UIA on every turn.
#[cfg(feature = "local-host")]
fn with_session_persona(prompt: &str, chosen: &str, baseline: Option<&str>) -> String {
    let chosen = chosen.trim();
    if chosen.is_empty() || prompt.contains(chosen) {
        return prompt.to_owned();
    }
    let role = format!(
        "## Deine UIA in diesem Chat\n\nDie folgende UIA ist deine Identität in dieser Session. Sprich in ihrer Ich-Form und folge ihren Anweisungen.\n\n{chosen}"
    );
    if let Some(previous) = baseline.map(str::trim).filter(|text| !text.is_empty())
        && prompt.contains(previous)
    {
        return prompt.replacen(previous, &role, 1);
    }
    format!("{prompt}\n\n{role}")
}

#[cfg(all(test, feature = "local-host"))]
#[test]
fn a_session_persona_replaces_the_original_uias_identity_only() {
    let prompt = "Host rules\n\nDu bist Atlas.\n\nWorkspace instructions";
    let changed = with_session_persona(prompt, "Du bist Suri.", Some("Du bist Atlas."));
    assert!(changed.contains("Host rules") && changed.contains("Workspace instructions"));
    assert!(changed.contains("Du bist Suri.") && !changed.contains("Du bist Atlas."));
}

/// Stand-in while the next model loads; never answers.
#[cfg(feature = "local-host")]
struct NoModel;

#[cfg(feature = "local-host")]
impl harw_core::ModelProvider for NoModel {
    fn respond<'a>(&'a self, _request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        Box::pin(async {
            Err(harw_core::ModelError::RequestFailed(
                "the on-device model is not loaded".to_owned(),
            ))
        })
    }
}

/// Tools a cloud route on the phone keeps visible: the phone UIA's set and the
/// device tools (the latter only exist when device tools are mounted).
#[cfg(feature = "local-host")]
const LEAN_TOOLS: &[&str] = &[
    "fs.read",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
    "fs.write",
    "fs.edit",
    "plan.current",
    "skills.search",
    "skills.load",
    "device_roots",
    "device_list",
    "device_read",
    "device_write",
    "device_search",
    "termux_run",
];

/// Explicit, validated chat binding, otherwise the current profile default.
#[cfg(feature = "local-host")]
fn resolve_chat_route(
    home: &crate::home::AppHome,
    session_id: Option<&str>,
) -> Result<Option<(String, String)>, MobileError> {
    if let Some(id) = session_id
        && let Some(selection) = crate::uia::chat_model(home, id)?
    {
        return crate::providers::validated_chat_route(home, &selection).map(Some);
    }
    home.active_route()
}

/// Build the captured selection without modifying the profile's route.
#[cfg(feature = "local-host")]
fn build_selected_model(
    home: &crate::home::AppHome,
    resolver: &crate::secrets::KeystoreSecretResolver,
    selection: Option<&(String, String)>,
) -> Result<CachedProvider, MobileError> {
    match selection {
        Some((provider, model_id)) if provider == crate::home::LOCAL_ROUTE_ID => build_local_gguf(home, model_id),
        Some(selection) => crate::providers::build_model_provider_for_route(home, resolver, selection).map(CachedProvider::Other),
        None => Ok(CachedProvider::Other(Arc::new(NoModel))),
    }
}

/// Settings of the on-device model: `HARW_LOCAL_N_CTX` sets the context
/// window (default 16384), `HARW_LOCAL_IMAGE_TOKENS` the most tokens one image
/// may cost (default 256; larger reads small text better but is slower),
/// `HARW_LOCAL_LEAN=0` brings back the full host
/// prompt and tool specs (text-only by default).
#[cfg(feature = "local-host")]
fn local_gguf_config() -> harw_llama::LocalGgufConfig {
    let mut config = harw_llama::LocalGgufConfig {
        inference: (*LOCAL_INFERENCE).clone(),
        ..harw_llama::LocalGgufConfig::default()
    };
    if let Some(n) = std::env::var("HARW_LOCAL_N_CTX")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|n| *n >= 1024)
    {
        config.n_ctx = n;
    }
    if let Some(n) = std::env::var("HARW_LOCAL_IMAGE_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .filter(|n| (16..=4096).contains(n))
    {
        config.image_max_tokens = n;
    }
    if std::env::var("HARW_LOCAL_LEAN").is_ok_and(|v| v.trim() == "0") {
        config.lean = false;
    }
    config
}

/// Loads the selected GGUF from the model store (memory-mapped, blocking).
#[cfg(feature = "local-host")]
fn build_local_gguf(
    home: &crate::home::AppHome,
    model_id: &str,
) -> Result<CachedProvider, MobileError> {
    build_local_gguf_profile(home, model_id, true, true)
}

#[cfg(feature = "local-host")]
fn build_local_assistant_gguf(
    home: &crate::home::AppHome,
    model_id: &str,
) -> Result<CachedProvider, MobileError> {
    build_local_gguf_profile(home, model_id, false, false)
}

#[cfg(feature = "local-host")]
fn build_local_gguf_profile(
    home: &crate::home::AppHome,
    model_id: &str,
    coding_profile: bool,
    needs_projector: bool,
) -> Result<CachedProvider, MobileError> {
    let session = |detail: String| MobileError::Session { detail };
    if !harw_llama::engine_available() {
        return Err(session(
            "on-device models are not available in this build".to_owned(),
        ));
    }
    let models_dir = home.local_models_dir().ok_or_else(|| {
        session("the on-device model folder is unknown; choose the model again".to_owned())
    })?;
    // Fails unless the file is complete and readable (e.g. deleted since selecting).
    let vision = needs_projector
        .then(|| crate::models::vision_path_in(std::path::Path::new(&models_dir), model_id))
        .flatten();
    if needs_projector && !coding_profile && vision.is_none() {
        return Err(session("the private assistant needs a local vision projector".into()));
    }
    let path = crate::models::model_path(models_dir, model_id.to_owned())?;
    let before = vision_files(&path, vision.as_deref());
    let mut config = local_gguf_config();
    let has_projector = vision.is_some();
    config.mmproj = vision.as_deref().map(Into::into);
    if !coding_profile {
        // The ordinary lean profile substitutes its own system prompt. The
        // tool-free room must keep its explicit personal-assistant prompt.
        config.lean = false;
    }
    let provider =
        harw_llama::LocalGgufProvider::open(std::path::Path::new(&path), model_id, config)
            .map_err(|e| session(e.to_string()))?;
    if coding_profile {
        crate::models::set_local_vision_status(crate::models::LocalVisionStatus {
            model_id: model_id.to_owned(),
            projector_present: has_projector,
            loaded: provider.supports_vision(),
            error: provider.vision_error().map(str::to_owned),
        });
    }
    let vision_files = before.filter(|checked| {
        provider.supports_vision()
            && vision_files(&path, vision.as_deref()).as_ref() == Some(checked)
    });
    Ok(CachedProvider::Local {
        model: Arc::new(provider),
        vision_files,
        loaded_revision: MODEL_REVISIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    })
}

/// Starts the host over `model` without device tools.
#[cfg(feature = "local-host")]
pub(crate) async fn open_local_with_model(
    home: crate::home::AppHome,
    model: Arc<dyn harw_core::ModelProvider>,
    features: Vec<String>,
) -> Result<Backend, MobileError> {
    open_local_with_tools(home, model, features, Vec::new(), None).await
}

/// Starts the host over `model` with extra tool contributors, connects as its
/// owner and says hello.
#[cfg(feature = "local-host")]
async fn open_local_with_tools(
    home: crate::home::AppHome,
    model: Arc<dyn harw_core::ModelProvider>,
    features: Vec<String>,
    contributors: Vec<Arc<dyn harw_local_host::ext::AssemblyContributor>>,
    surface: Option<harw_local_host::ToolSurface>,
) -> Result<Backend, MobileError> {
    use harw_protocol::{
        SessionPort,
        session_wire::{HelloParams, SESSION_WIRE_MINOR},
    };

    let config = home.local_host_config();
    let projects_root = Some(home.projects_dir());
    let host = tokio::task::spawn_blocking(move || {
        harw_local_host::LocalHost::open_phone_with_projects(
            config,
            model,
            contributors,
            surface,
            projects_root,
        )
    })
    .await
    .map_err(|_| host_task_failed())?
    .map_err(|e| MobileError::Internal {
        detail: e.to_string(),
    })?;
    let connection = match host.connect_owner() {
        Ok(connection) => connection,
        Err(e) => {
            host.shutdown();
            return Err(MobileError::Internal {
                detail: e.to_string(),
            });
        }
    };
    let hello = SessionPort::hello(
        &connection,
        HelloParams {
            client_label: LOCAL_CLIENT_LABEL.into(),
            wire_minor: SESSION_WIRE_MINOR,
            features,
            requested_caps: None,
        },
    )
    .await;
    let ack = match hello {
        Ok(ack) => ack,
        Err(e) => {
            host.shutdown();
            return Err(MobileError::from(e));
        }
    };
    Ok(Backend::local(
        Arc::new(connection),
        ack,
        Box::new(LocalGuard {
            host: std::sync::Mutex::new(host),
        }),
    ))
}

/// Keeps the host alive for the backend's lifetime and drains it on drop.
#[cfg(feature = "local-host")]
struct LocalGuard {
    host: std::sync::Mutex<harw_local_host::LocalHost>,
}

#[cfg(feature = "local-host")]
impl Drop for LocalGuard {
    fn drop(&mut self) {
        let host = self
            .host
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        host.shutdown();
    }
}

#[cfg(all(test, feature = "local-host"))]
mod tests {
    use harw_protocol::session_wire::CreateParams;

    use super::*;
    use crate::{backend::BackendKind, home::AppHome};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn model() -> Arc<dyn harw_core::ModelProvider> {
        Arc::new(harw_core::EchoModelProvider::new("hi"))
    }

    #[cfg(feature = "vault-uia-chat")]
    #[test]
    fn local_uia_scope_is_independent_of_model_availability() -> TestResult {
        let app = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        let home = AppHome::open(app.path())?;
        let key = uia_home_key(&home);
        assert!(matches_uia_home(&home, app.path()));
        assert!(!matches_uia_home(&home, other.path()));
        assert_ne!(key, uia_home_key(&AppHome::open(other.path())?));

        home.set_active_route("example-api", "example-model")?;
        assert_eq!(home.active_local_model()?, None);
        assert_eq!(uia_home_key(&home), key);

        home.set_active_route(crate::home::LOCAL_ROUTE_ID, "missing-gguf")?;
        assert!(home.local_models_dir().is_none());
        assert!(matches_uia_home(&home, app.path()));
        assert_eq!(uia_home_key(&home), key);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_backend_creates_and_lists_a_session() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        let backend = open_local_with_model(home, model(), Vec::new()).await?;
        assert_eq!(backend.kind(), BackendKind::Local);
        let granted = backend.granted();
        assert!(granted.steer && granted.approve);
        let summary = backend
            .port()
            .create(CreateParams {
                workspace: None,
                title: Some("t".into()),
                fork_from: None,
            })
            .await
            .map_err(MobileError::from)?;
        let listed = backend.port().list().await.map_err(MobileError::from)?;
        assert!(listed.iter().any(|s| s.session_id == summary.session_id));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_backend_reopens_with_existing_sessions() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        let backend = open_local_with_model(home.clone(), model(), Vec::new()).await?;
        let summary = backend
            .port()
            .create(CreateParams {
                workspace: None,
                title: Some("t".into()),
                fork_from: None,
            })
            .await
            .map_err(MobileError::from)?;
        drop(backend);
        let reopened = open_local_with_model(home, model(), Vec::new()).await?;
        let listed = reopened.port().list().await.map_err(MobileError::from)?;
        assert!(listed.iter().any(|s| s.session_id == summary.session_id));
        Ok(())
    }
}

#[cfg(all(test, not(feature = "local-host")))]
mod tests {
    use super::*;
    use crate::secrets::test_support::MemoryKeystore;

    #[tokio::test]
    async fn open_local_is_unavailable_without_the_feature() {
        let result = open_local(
            PathBuf::from("/nonexistent"),
            MemoryKeystore::new(),
            Vec::new(),
            (None, None),
        )
        .await;
        assert!(matches!(
            result.err(),
            Some(MobileError::Session { detail }) if detail == LOCAL_HOST_UNAVAILABLE
        ));
    }
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) fn uia_profile_key(files: &std::path::Path) -> Option<String> {
    Some(uia_home_key(&uia_runtime_for(files)?.home))
}

/// Pin the selected profile of the actual model runtime being admitted,
/// not of a replacement runtime that could have raced with lookup.
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) fn uia_runtime_profile_key(
    runtime: &SwitchingModel, files: &std::path::Path,
) -> Option<String> {
    matches_uia_home(&runtime.home, files).then(|| uia_home_key(&runtime.home))
}

/// Opening an authenticated room or Library is a storage operation, not a
/// model request. A missing, changed, or API model must not lock the vault or
/// change its owner. Send and caption retain their independent model checks.
#[cfg(all(feature = "vault-uia-chat", any(target_os = "android", test)))]
fn uia_home_key(home: &crate::home::AppHome) -> String {
    // No raw local profile, path or cloud identity exported to Kotlin.
    // Keep the original derivation domain so opted-in installations reopen.
    let mut key = blake3::Hasher::new_derive_key("harw local uia development profile registration v1");
    key.update(home.profile_dir().as_os_str().as_encoded_bytes());
    key.finalize().to_hex().to_string()
}
