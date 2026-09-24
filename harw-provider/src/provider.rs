//! Provider als Hauptobjekt + Builder (Note 12 §5, §7, §8, §13, §20).
//!
//! `Provider<RoleTag, KindTag, AuthTag, StateTag>` ist die typisierte Form;
//! `ProviderRecord` die erased Runtime-Form, die in der Registry landet.
//! `Primary`/`Secondary` sind Tags am Provider — kein separates Fallback-Objekt.

use std::marker::PhantomData;
use std::sync::Arc;

use harw_types::{CustomerId, ProviderId, ProviderName};
use url::Url;

use crate::auth::ProviderAuth;
use crate::error::{ProviderError, ProviderResult};
use crate::marker::{
    ApiKeyAuth, OpenAiCompat, ProviderRoleTag, Registered, RoleMarker, Secondary, Unregistered,
};
use crate::model::ModelRecord;

/// Provider-weite Settings (Note 12 §5).
#[derive(Clone, Debug, Default)]
pub struct ProviderSettings {
    /// Typisierte Beschreibung des Request-Zeitlimits. Der produktive
    /// HTTP-Pfad (`harw-provider-http`) liest das wirksame Zeitlimit aus
    /// `harw_config::ProviderToml::request_timeout_secs` bzw.
    /// `stream_idle_timeout_secs` (Runde 7, Teil L4), nicht aus diesem Feld.
    pub request_timeout: Option<std::time::Duration>,
    pub connect_timeout: Option<std::time::Duration>,
    pub max_retries: Option<u32>,
    pub max_concurrency: Option<u32>,
    pub customer_id: Option<CustomerId>,
    pub enabled: Option<bool>,
    pub metadata: ProviderMetadata,
}

/// Beschreibende Metadaten eines Providers.
#[derive(Clone, Debug, Default)]
pub struct ProviderMetadata {
    pub label: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub owner: Option<Arc<str>>,
    pub environment: Option<Arc<str>>,
}

/// Provider als Hauptobjekt mit vier Typestate-Achsen.
#[derive(Clone, Debug)]
pub struct Provider<
    RoleTag = Secondary,
    KindTag = OpenAiCompat,
    AuthTag = ApiKeyAuth,
    StateTag = Unregistered,
> {
    pub id: ProviderId,
    pub name: ProviderName,
    pub base_url: Url,
    pub auth: ProviderAuth,
    pub settings: ProviderSettings,
    pub models: Vec<ModelRecord>,
    pub fallback_tags: Vec<ProviderRoleTag>,
    _role: PhantomData<RoleTag>,
    _kind: PhantomData<KindTag>,
    _auth: PhantomData<AuthTag>,
    _state: PhantomData<StateTag>,
}

impl<RoleTag, KindTag, AuthTag, StateTag> Provider<RoleTag, KindTag, AuthTag, StateTag> {
    #[must_use]
    pub fn id(&self) -> &ProviderId {
        &self.id
    }
    #[must_use]
    pub fn name(&self) -> &ProviderName {
        &self.name
    }
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }
    #[must_use]
    pub fn auth(&self) -> &ProviderAuth {
        &self.auth
    }
    #[must_use]
    pub fn settings(&self) -> &ProviderSettings {
        &self.settings
    }
    #[must_use]
    pub fn models(&self) -> &[ModelRecord] {
        &self.models
    }
    #[must_use]
    pub fn fallback_tags(&self) -> &[ProviderRoleTag] {
        &self.fallback_tags
    }
}

impl<RoleTag, KindTag, AuthTag, StateTag> Provider<RoleTag, KindTag, AuthTag, StateTag>
where
    RoleTag: RoleMarker,
{
    /// Erased Runtime-Form mit aufgelöstem Rollen-Tag.
    #[must_use]
    pub fn as_record(&self) -> ProviderRecord {
        ProviderRecord {
            id: self.id.clone(),
            name: self.name.clone(),
            base_url: self.base_url.clone(),
            auth: self.auth.clone(),
            role: RoleTag::ROLE,
            settings: self.settings.clone(),
            models: self.models.clone(),
            fallback_tags: self.fallback_tags.clone(),
        }
    }
}

/// Erased Runtime-Form eines Providers (Note 12 §8).
#[derive(Clone, Debug)]
pub struct ProviderRecord {
    pub id: ProviderId,
    pub name: ProviderName,
    pub base_url: Url,
    pub auth: ProviderAuth,
    pub role: ProviderRoleTag,
    pub settings: ProviderSettings,
    pub models: Vec<ModelRecord>,
    pub fallback_tags: Vec<ProviderRoleTag>,
}

impl ProviderRecord {
    #[must_use]
    pub fn is_primary(&self) -> bool {
        self.role == ProviderRoleTag::Primary
    }
}

/// Trait-Schicht für typisierte Provider (Note 12 §9, §20).
pub trait ProviderLike: Send + Sync + 'static {
    type Role: RoleMarker;
    type Kind;
    type Auth;
    type State;

    fn as_record(&self) -> ProviderRecord;
    fn id(&self) -> &ProviderId;
    fn name(&self) -> &ProviderName;
    fn base_url(&self) -> &Url;
    fn auth(&self) -> &ProviderAuth;
    fn settings(&self) -> &ProviderSettings;
    fn models(&self) -> &[ModelRecord];
    fn fallback_tags(&self) -> &[ProviderRoleTag];
}

impl<RoleTag, KindTag, AuthTag> ProviderLike for Provider<RoleTag, KindTag, AuthTag, Registered>
where
    RoleTag: RoleMarker + Send + Sync + 'static,
    KindTag: Send + Sync + 'static,
    AuthTag: Send + Sync + 'static,
{
    type Role = RoleTag;
    type Kind = KindTag;
    type Auth = AuthTag;
    type State = Registered;

    fn as_record(&self) -> ProviderRecord {
        Provider::<RoleTag, KindTag, AuthTag, Registered>::as_record(self)
    }
    fn id(&self) -> &ProviderId {
        &self.id
    }
    fn name(&self) -> &ProviderName {
        &self.name
    }
    fn base_url(&self) -> &Url {
        &self.base_url
    }
    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }
    fn settings(&self) -> &ProviderSettings {
        &self.settings
    }
    fn models(&self) -> &[ModelRecord] {
        &self.models
    }
    fn fallback_tags(&self) -> &[ProviderRoleTag] {
        &self.fallback_tags
    }
}

impl<RoleTag, KindTag, AuthTag> From<Provider<RoleTag, KindTag, AuthTag, Registered>>
    for ProviderRecord
where
    RoleTag: RoleMarker,
{
    fn from(value: Provider<RoleTag, KindTag, AuthTag, Registered>) -> Self {
        ProviderRecord {
            id: value.id,
            name: value.name,
            base_url: value.base_url,
            auth: value.auth,
            role: RoleTag::ROLE,
            settings: value.settings,
            models: value.models,
            fallback_tags: value.fallback_tags,
        }
    }
}

/// Provider-Builder mit Rollen-Transitions (Note 12 §13).
#[derive(Clone, Debug)]
pub struct ProviderBuilder<
    RoleTag = Secondary,
    KindTag = OpenAiCompat,
    AuthTag = ApiKeyAuth,
    StateTag = Unregistered,
> {
    id: Option<ProviderId>,
    name: Option<ProviderName>,
    base_url: Option<Url>,
    auth: Option<ProviderAuth>,
    settings: Option<ProviderSettings>,
    models: Vec<ModelRecord>,
    fallback_tags: Vec<ProviderRoleTag>,
    _role: PhantomData<RoleTag>,
    _kind: PhantomData<KindTag>,
    _auth: PhantomData<AuthTag>,
    _state: PhantomData<StateTag>,
}

impl Default for ProviderBuilder<Secondary, OpenAiCompat, ApiKeyAuth, Unregistered> {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderBuilder<Secondary, OpenAiCompat, ApiKeyAuth, Unregistered> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: None,
            name: None,
            base_url: None,
            auth: None,
            settings: None,
            models: Vec::new(),
            fallback_tags: Vec::new(),
            _role: PhantomData,
            _kind: PhantomData,
            _auth: PhantomData,
            _state: PhantomData,
        }
    }
}

impl<RoleTag, KindTag, AuthTag, StateTag> ProviderBuilder<RoleTag, KindTag, AuthTag, StateTag> {
    #[must_use]
    pub fn id(mut self, id: ProviderId) -> Self {
        self.id = Some(id);
        self
    }
    #[must_use]
    pub fn name(mut self, name: ProviderName) -> Self {
        self.name = Some(name);
        self
    }
    #[must_use]
    pub fn base_url(mut self, base_url: Url) -> Self {
        self.base_url = Some(base_url);
        self
    }
    #[must_use]
    pub fn auth(mut self, auth: ProviderAuth) -> Self {
        self.auth = Some(auth);
        self
    }
    #[must_use]
    pub fn settings(mut self, settings: ProviderSettings) -> Self {
        self.settings = Some(settings);
        self
    }
    #[must_use]
    pub fn add_model(mut self, model: ModelRecord) -> Self {
        self.models.push(model);
        self
    }
    #[must_use]
    pub fn add_models<I>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = ModelRecord>,
    {
        self.models.extend(models);
        self
    }
    #[must_use]
    pub fn fallback_tag(mut self, role: ProviderRoleTag) -> Self {
        self.fallback_tags.push(role);
        self
    }
    #[must_use]
    pub fn fallback_tags<I>(mut self, roles: I) -> Self
    where
        I: IntoIterator<Item = ProviderRoleTag>,
    {
        self.fallback_tags.extend(roles);
        self
    }

    fn retag_role<NewRole>(self) -> ProviderBuilder<NewRole, KindTag, AuthTag, StateTag> {
        ProviderBuilder {
            id: self.id,
            name: self.name,
            base_url: self.base_url,
            auth: self.auth,
            settings: self.settings,
            models: self.models,
            fallback_tags: self.fallback_tags,
            _role: PhantomData,
            _kind: PhantomData,
            _auth: PhantomData,
            _state: PhantomData,
        }
    }
}

impl<KindTag, AuthTag, StateTag> ProviderBuilder<Secondary, KindTag, AuthTag, StateTag> {
    /// Markiert den Provider als Primary (Typestate-Transition).
    #[must_use]
    pub fn primary(self) -> ProviderBuilder<crate::marker::Primary, KindTag, AuthTag, StateTag> {
        self.retag_role()
    }
}

impl<KindTag, AuthTag, StateTag>
    ProviderBuilder<crate::marker::Primary, KindTag, AuthTag, StateTag>
{
    /// Markiert den Provider als Secondary (Typestate-Transition).
    #[must_use]
    pub fn secondary(self) -> ProviderBuilder<Secondary, KindTag, AuthTag, StateTag> {
        self.retag_role()
    }
}

impl<RoleTag, KindTag, AuthTag> ProviderBuilder<RoleTag, KindTag, AuthTag, Unregistered>
where
    RoleTag: RoleMarker,
{
    /// Validiert Pflichtfelder und baut den registrierten Provider.
    pub fn build(self) -> ProviderResult<Provider<RoleTag, KindTag, AuthTag, Registered>> {
        let id = self.id.ok_or(ProviderError::MissingProviderField("id"))?;
        let name = self
            .name
            .ok_or(ProviderError::MissingProviderField("name"))?;
        let base_url = self
            .base_url
            .ok_or(ProviderError::MissingProviderField("base_url"))?;
        let auth = self
            .auth
            .ok_or(ProviderError::MissingProviderField("auth"))?;

        Ok(Provider {
            id,
            name,
            base_url,
            auth,
            settings: self.settings.unwrap_or_default(),
            models: self.models,
            fallback_tags: self.fallback_tags,
            _role: PhantomData,
            _kind: PhantomData,
            _auth: PhantomData,
            _state: PhantomData,
        })
    }

    /// Wie [`Self::build`], liefert aber direkt den erased `ProviderRecord`.
    pub fn build_record(self) -> ProviderResult<ProviderRecord> {
        Ok(self.build()?.into())
    }
}
