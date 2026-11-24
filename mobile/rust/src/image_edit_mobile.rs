//! Single-image Android edit bridge. A draft is a native one-shot capability
//! for one exact device provider authority, selected route, credential revision, PNG, and prompt.

use crate::MobileError;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ApiImageEditChoice {
    pub route_id: String,
    pub model_id: String,
    pub endpoint: String,
}

/// Owned, bounded output bytes. Never contains an externally fetchable URL.
#[derive(uniffi::Record)]
pub struct ApiImageEditAsset {
    pub png: Vec<u8>,
    pub mime: String,
    pub content_digest: String,
    pub width: u32,
    pub height: u32,
}

#[derive(uniffi::Object)]
pub struct ApiImageEditDraft {
    #[cfg(feature = "local-host")]
    inner: std::sync::Mutex<Option<binding::Prepared>>,
}

#[uniffi::export]
impl ApiImageEditDraft {
    /// The exact metadata-stripped bytes which the approval would upload.
    pub fn preview_png(&self) -> Result<Vec<u8>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let draft = self.inner.lock().map_err(|_| unavailable())?;
            draft
                .as_ref()
                .map(|item| item.preview_png().to_vec())
                .ok_or_else(unavailable)
        }
        #[cfg(not(feature = "local-host"))]
        {
            Err(unavailable())
        }
    }

    /// The trimmed edit instruction in the immutable request preview.
    pub fn instruction(&self) -> Result<String, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let draft = self.inner.lock().map_err(|_| unavailable())?;
            draft
                .as_ref()
                .map(|item| item.instruction().to_owned())
                .ok_or_else(unavailable)
        }
        #[cfg(not(feature = "local-host"))]
        {
            Err(unavailable())
        }
    }
}

pub(crate) fn unavailable() -> MobileError {
    session("Bildbearbeitung ist auf diesem Gerät nicht verfügbar.")
}

fn session(detail: &'static str) -> MobileError {
    MobileError::Session {
        detail: detail.into(),
    }
}

#[cfg(feature = "local-host")]
pub(crate) mod binding {
    use std::sync::{Arc, Mutex, Weak};

    use harw_config::{ProviderToml, ResolvedConfig, SecretRef};
    use secrecy::{ExposeSecret, SecretString};

    use super::{ApiImageEditAsset, ApiImageEditChoice, ApiImageEditDraft, session, unavailable};
    use crate::{
        MobileError,
        image_edit::{
            ConfiguredEditRoute, ImageEditError, ImageEditRoute, PreparedImageEdit,
            edit_confirmed_synchronous,
        },
        home::AppHome,
        local::SwitchingModel,
        secrets::{KeystoreSecretResolver, route_slot},
    };

    static EDIT_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

    #[derive(Clone)]
    pub(crate) enum EditOwner {
        Backend(Weak<SwitchingModel>),
        Store(Weak<()>),
    }

    impl EditOwner {
        fn same(&self, other: &Self) -> bool {
            match (self, other) {
                (Self::Backend(left), Self::Backend(right)) => left.ptr_eq(right),
                (Self::Store(left), Self::Store(right)) => left.ptr_eq(right),
                _ => false,
            }
        }
    }

    pub(super) struct Prepared {
        owner: EditOwner,
        choice: ApiImageEditChoice,
        credential: blake3::Hash,
        approved: PreparedImageEdit,
    }

    impl Prepared {
        pub(super) fn preview_png(&self) -> &[u8] {
            self.approved.source()
        }

        pub(super) fn instruction(&self) -> &str {
            self.approved.instruction()
        }
    }

    fn config(home: &AppHome) -> Result<ResolvedConfig, MobileError> {
        let active = harw_home::active_profile_name(home.home());
        let active_dir = harw_home::profile_dir(home.home(), &active).map_err(|_| unavailable())?;
        if active_dir != home.profile_dir() {
            return Err(route_changed());
        }
        let layers = harw_home::config_layers_report_at(home.home(), home.workspace())
            .map_err(|_| unavailable())?
            .layers;
        harw_config::discover_config(&layers).map_err(|_| unavailable())
    }

    fn route_changed() -> MobileError {
        session("Die gewählte API-Route ist nicht mehr verfügbar oder hat sich geändert.")
    }

    fn configured<'a>(id: &'a str, provider: &'a ProviderToml) -> ConfiguredEditRoute<'a> {
        ConfiguredEditRoute {
            route_id: id,
            api: &provider.api,
            base_url: &provider.base_url,
            available_models: &provider.models,
            enabled: provider.enabled,
            // The bridge additionally pins the actual route API key digest below;
            // the config snapshot itself is re-resolved before every send.
            revision: 0,
        }
    }

    fn api_slot(id: &str, provider: &ProviderToml) -> Option<String> {
        let slot = route_slot(id);
        (provider.auth.as_ref() == Some(&SecretRef::Secrets(slot.clone()))
            && matches!(provider.auth_header.as_deref(), None | Some("bearer"))
            && provider.api_key.as_ref().is_none_or(|value| value.is_empty())
            // This standalone endpoint does not interpret or forward custom headers.
            // Reject their presence rather than silently changing project or org scope.
            && provider.headers.is_empty())
        .then_some(slot)
    }

    fn choice_for(
        secrets: &KeystoreSecretResolver,
        id: &str,
        provider: &ProviderToml,
    ) -> Option<(ImageEditRoute, ApiImageEditChoice, String)> {
        let slot = api_slot(id, provider)?;
        if !secrets.has(&slot) {
            return None;
        }
        let route = ImageEditRoute::select(&configured(id, provider), id, "gpt-image-1.5").ok()?;
        let visible = ApiImageEditChoice {
            route_id: id.to_owned(),
            model_id: route.model().into(),
            endpoint: route.endpoint().into(),
        };
        Some((route, visible, slot))
    }

    pub(crate) fn choices(model: &SwitchingModel) -> Result<Vec<ApiImageEditChoice>, MobileError> {
        choices_for(model.home(), model.resolver())
    }

    pub(crate) fn choices_for(
        home: &AppHome,
        secrets: &KeystoreSecretResolver,
    ) -> Result<Vec<ApiImageEditChoice>, MobileError> {
        let config = config(home)?;
        let mut choices: Vec<_> = config
            .providers
            .iter()
            .filter_map(|(id, provider)| choice_for(secrets, id, provider).map(|(_, row, _)| row))
            .collect();
        choices.sort_by(|left, right| left.route_id.cmp(&right.route_id));
        Ok(choices)
    }

    fn resolve_key(secrets: &KeystoreSecretResolver, slot: &str) -> Result<SecretString, MobileError> {
        secrets
            .resolve_slot(slot)
            .map_err(|_| session("Der API-Schlüssel der gewählten Route ist nicht verfügbar."))
    }

    fn fingerprint(key: &SecretString) -> blake3::Hash {
        // Native-only, in-memory and scoped to this draft. Never exposed to FFI or logs.
        blake3::hash(key.expose_secret().as_bytes())
    }

    pub(crate) fn prepare(
        model: &Arc<SwitchingModel>,
        choice: ApiImageEditChoice,
        png: Vec<u8>,
        instruction: String,
    ) -> Result<Arc<ApiImageEditDraft>, MobileError> {
        prepare_for(
            model.home(),
            model.resolver(),
            EditOwner::Backend(Arc::downgrade(model)),
            choice,
            png,
            instruction,
        )
    }

    pub(crate) fn prepare_for(
        home: &AppHome,
        secrets: &KeystoreSecretResolver,
        owner: EditOwner,
        choice: ApiImageEditChoice,
        png: Vec<u8>,
        instruction: String,
    ) -> Result<Arc<ApiImageEditDraft>, MobileError> {
        let png = zeroize::Zeroizing::new(png);
        let instruction = zeroize::Zeroizing::new(instruction);
        let config = config(home)?;
        let provider = config
            .providers
            .get(&choice.route_id)
            .ok_or_else(route_changed)?;
        let (route, current, slot) =
            choice_for(secrets, &choice.route_id, provider).ok_or_else(route_changed)?;
        if current != choice {
            return Err(route_changed());
        }
        let approved = PreparedImageEdit::prepare(route, &png, &instruction).map_err(edit_error)?;
        let credential = fingerprint(&resolve_key(secrets, &slot)?);
        Ok(Arc::new(ApiImageEditDraft {
            inner: Mutex::new(Some(Prepared {
                owner,
                choice,
                credential,
                approved,
            })),
        }))
    }

    pub(crate) async fn confirm(
        model: Arc<SwitchingModel>,
        draft: Arc<ApiImageEditDraft>,
        explicitly_synchronous: bool,
    ) -> Result<ApiImageEditAsset, MobileError> {
        confirm_for(
            model.home(),
            model.resolver(),
            EditOwner::Backend(Arc::downgrade(&model)),
            draft,
            explicitly_synchronous,
        )
        .await
    }

    pub(crate) async fn confirm_for(
        home: &AppHome,
        secrets: &KeystoreSecretResolver,
        owner: EditOwner,
        draft: Arc<ApiImageEditDraft>,
        explicitly_synchronous: bool,
    ) -> Result<ApiImageEditAsset, MobileError> {
        if !explicitly_synchronous {
            return Err(session(
                "Bestätige ausdrücklich die synchrone Einzelanfrage.",
            ));
        }
        let _permit = EDIT_GATE
            .try_acquire()
            .map_err(|_| session("Eine API-Bildbearbeitung läuft bereits."))?;
        let prepared = {
            let mut slot = draft.inner.lock().map_err(|_| unavailable())?;
            let prepared = slot
                .as_ref()
                .ok_or_else(|| session("Diese Bildvorschau wurde schon verwendet."))?;
            if !prepared.owner.same(&owner) {
                return Err(route_changed());
            }
            slot.take().ok_or_else(unavailable)?
        };
        let config = config(home)?;
        let provider = config
            .providers
            .get(&prepared.choice.route_id)
            .ok_or_else(route_changed)?;
        let (_, current, slot) =
            choice_for(secrets, &prepared.choice.route_id, provider).ok_or_else(route_changed)?;
        if current != prepared.choice {
            return Err(route_changed());
        }
        let key = resolve_key(secrets, &slot)?;
        if fingerprint(&key) != prepared.credential {
            return Err(route_changed());
        }

        let current_config = configured(&prepared.choice.route_id, provider);
        let route_id = current_config.route_id;
        let edited = edit_confirmed_synchronous(prepared.approved, &current_config, |id| {
            if id != route_id {
                return Err(ImageEditError::RouteChanged);
            }
            Ok(key)
        })
        .await
        .map_err(edit_error)?;
        let (width, height) = edited.dimensions();
        Ok(ApiImageEditAsset {
            mime: edited.mime().into(),
            content_digest: edited.digest().to_hex().to_string(),
            png: edited.into_bytes(),
            width,
            height,
        })
    }

    fn edit_error(error: ImageEditError) -> MobileError {
        use ImageEditError as E;
        session(match error {
            E::RouteNotEligible | E::RouteChanged => {
                "Die gewählte API-Route ist nicht mehr verfügbar oder hat sich geändert."
            }
            E::InvalidInstruction => "Der Bearbeitungsauftrag ist leer, zu lang oder ungültig.",
            E::InvalidImage => "Das Bild ist kein unterstütztes PNG.",
            E::IncompleteImage => "Die Bilddatei endete vorzeitig. Kein Bild wurde übernommen.",
            E::MissingCredential => "Der API-Schlüssel der gewählten Route ist nicht verfügbar.",
            E::BuildRequest => "Die Bildanfrage konnte nicht erstellt werden.",
            E::ProviderRefused => "Der gewählte Anbieter hat kein bearbeitetes Bild geliefert.",
            E::UnknownDelivery => "Die Zustellung ist nicht bestätigt. Harw sendet nichts erneut.",
            E::ResponseInterrupted => "Die Antwort endete vorzeitig. Kein Bild wurde übernommen.",
            E::ResponseType | E::InvalidResponse => {
                "Der Anbieter hat kein gültiges bearbeitetes Bild geliefert."
            }
            E::ResponseTooLarge => "Die Antwort überschreitet das Bildlimit.",
        })
    }
}
