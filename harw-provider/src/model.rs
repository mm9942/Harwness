//! Model-Definition als Hauptobjekt + Builder (Note 12 §5, §6, §14).
//!
//! `ModelDefinition<CapabilityTag>` trägt die Fähigkeit zur Compile-Zeit;
//! `ModelRecord` ist die erased Runtime-Form mit `ModelCapabilityTag`.

use std::marker::PhantomData;
use std::sync::Arc;

use harw_types::{ModelId, ModelName};

use crate::error::{ProviderError, ProviderResult};
use crate::marker::{
    AudioModel, ChatModel, EmbeddingModel, ModelCapabilityMarker, ModelCapabilityTag,
    ReasoningModel, ToolCallingModel, VisionModel,
};

/// Model-Settings (Note 12 §5).
#[derive(Clone, Debug, Default)]
pub struct ModelSettings {
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub supports_streaming: Option<bool>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub metadata: ModelMetadata,
}

/// Beschreibende Metadaten eines Modells.
#[derive(Clone, Debug, Default)]
pub struct ModelMetadata {
    pub label: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
}

/// Model-Definition als Hauptobjekt, Fähigkeit per Typestate-Tag.
#[derive(Clone, Debug)]
pub struct ModelDefinition<CapabilityTag = ChatModel> {
    pub id: ModelId,
    pub name: ModelName,
    pub settings: ModelSettings,
    _capability: PhantomData<CapabilityTag>,
}

impl<CapabilityTag> ModelDefinition<CapabilityTag> {
    #[must_use]
    pub fn new(id: ModelId, name: ModelName, settings: ModelSettings) -> Self {
        Self {
            id,
            name,
            settings,
            _capability: PhantomData,
        }
    }

    #[must_use]
    pub fn id(&self) -> &ModelId {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &ModelName {
        &self.name
    }

    #[must_use]
    pub fn settings(&self) -> &ModelSettings {
        &self.settings
    }
}

impl<CapabilityTag: ModelCapabilityMarker> ModelDefinition<CapabilityTag> {
    /// Projiziert die typisierte Definition auf den erased `ModelRecord`.
    #[must_use]
    pub fn as_record(&self) -> ModelRecord {
        ModelRecord {
            id: self.id.clone(),
            name: self.name.clone(),
            capability: CapabilityTag::CAPABILITY,
            settings: self.settings.clone(),
        }
    }
}

/// Erased Runtime-Form eines Modells (Note 12 §8).
#[derive(Clone, Debug)]
pub struct ModelRecord {
    pub id: ModelId,
    pub name: ModelName,
    pub capability: ModelCapabilityTag,
    pub settings: ModelSettings,
}

/// Trait-Schicht für Modelle (Note 12 §9).
pub trait ModelLike: Send + Sync + 'static {
    type Capability: ModelCapabilityMarker;

    fn as_record(&self) -> ModelRecord;
    fn id(&self) -> &ModelId;
    fn name(&self) -> &ModelName;
    fn settings(&self) -> &ModelSettings;
}

impl<CapabilityTag> ModelLike for ModelDefinition<CapabilityTag>
where
    CapabilityTag: ModelCapabilityMarker + Send + Sync + 'static,
{
    type Capability = CapabilityTag;

    fn as_record(&self) -> ModelRecord {
        ModelDefinition::<CapabilityTag>::as_record(self)
    }
    fn id(&self) -> &ModelId {
        &self.id
    }
    fn name(&self) -> &ModelName {
        &self.name
    }
    fn settings(&self) -> &ModelSettings {
        &self.settings
    }
}

impl<CapabilityTag: ModelCapabilityMarker> From<ModelDefinition<CapabilityTag>> for ModelRecord {
    fn from(value: ModelDefinition<CapabilityTag>) -> Self {
        ModelRecord {
            id: value.id,
            name: value.name,
            capability: CapabilityTag::CAPABILITY,
            settings: value.settings,
        }
    }
}

/// Model-Builder mit Capability-Transitions (Note 12 §14).
#[derive(Clone, Debug)]
pub struct ModelBuilder<CapabilityTag = ChatModel> {
    id: Option<ModelId>,
    name: Option<ModelName>,
    settings: Option<ModelSettings>,
    _capability: PhantomData<CapabilityTag>,
}

impl Default for ModelBuilder<ChatModel> {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelBuilder<ChatModel> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: None,
            name: None,
            settings: None,
            _capability: PhantomData,
        }
    }
}

impl<CapabilityTag> ModelBuilder<CapabilityTag> {
    #[must_use]
    pub fn id(mut self, id: ModelId) -> Self {
        self.id = Some(id);
        self
    }

    #[must_use]
    pub fn name(mut self, name: ModelName) -> Self {
        self.name = Some(name);
        self
    }

    #[must_use]
    pub fn settings(mut self, settings: ModelSettings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Wechselt das Capability-Tag, ohne Felder zu verlieren.
    fn retag<NewTag>(self) -> ModelBuilder<NewTag> {
        ModelBuilder {
            id: self.id,
            name: self.name,
            settings: self.settings,
            _capability: PhantomData,
        }
    }
}

impl<CapabilityTag: ModelCapabilityMarker> ModelBuilder<CapabilityTag> {
    /// Validiert Pflichtfelder und baut die typisierte Definition.
    pub fn build(self) -> ProviderResult<ModelDefinition<CapabilityTag>> {
        let id = self.id.ok_or(ProviderError::MissingModelField("id"))?;
        let name = self.name.ok_or(ProviderError::MissingModelField("name"))?;
        Ok(ModelDefinition::new(
            id,
            name,
            self.settings.unwrap_or_default(),
        ))
    }

    /// Wie [`Self::build`], liefert aber direkt den erased `ModelRecord`.
    pub fn build_record(self) -> ProviderResult<ModelRecord> {
        Ok(self.build()?.as_record())
    }
}

impl ModelBuilder<ChatModel> {
    #[must_use]
    pub fn embedding(self) -> ModelBuilder<EmbeddingModel> {
        self.retag()
    }
    #[must_use]
    pub fn vision(self) -> ModelBuilder<VisionModel> {
        self.retag()
    }
    #[must_use]
    pub fn audio(self) -> ModelBuilder<AudioModel> {
        self.retag()
    }
    #[must_use]
    pub fn reasoning(self) -> ModelBuilder<ReasoningModel> {
        self.retag()
    }
    #[must_use]
    pub fn tool_calling(self) -> ModelBuilder<ToolCallingModel> {
        self.retag()
    }
}
