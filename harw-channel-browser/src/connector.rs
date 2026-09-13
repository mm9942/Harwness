use harw_browser::selector::{Selector, Target};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use url::Url;

use crate::error::{ConfirmationIssue, Error, Result, SelectorIssue, SelectorStrategy};

const CONNECTOR_SCHEMA: &str = "harwness.browser-channel/v1";
const BROWSER_BINDING: &str = "thirtyfour.firefox-bidi@1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorDefinition {
    schema: String,
    id: String,
    version: String,
    browser_binding: String,
    start_url: String,
    origin_policy: OriginPolicyDefinition,
    profile: ProfileDefinition,
    conversation_list: SelectorBlock,
    message_container: SelectorBlock,
    message: MessageDefinition,
    composer: ComposerDefinition,
    confirmation: ConfirmationDefinition,
    activity: ActivityDefinition,
}

impl ConnectorDefinition {
    pub fn from_toml(source: &str) -> Result<Self> {
        toml::from_str(source).map_err(Error::from)
    }

    pub fn compile(self) -> Result<CompiledConnector> {
        require_non_empty(&self.id, "id")?;
        require_non_empty(&self.version, "version")?;
        if self.schema != CONNECTOR_SCHEMA {
            return Err(Error::UnsupportedSchema { found: self.schema });
        }
        if self.browser_binding != BROWSER_BINDING {
            return Err(Error::UnsupportedBrowserBinding {
                found: self.browser_binding,
            });
        }

        let start_url = parse_url("start_url", self.start_url)?;
        let origin_policy = self.origin_policy.compile()?;
        if !origin_policy.allows(&start_url) {
            return Err(Error::StartOriginNotAllowed {
                start_url: start_url.as_str().to_owned(),
            });
        }

        Ok(CompiledConnector {
            schema: self.schema,
            id: self.id,
            version: self.version,
            browser_binding: self.browser_binding,
            start_url,
            origin_policy,
            profile: self.profile.compile()?,
            conversation_list: self
                .conversation_list
                .compile("conversation_list.selector")?,
            message_container: self
                .message_container
                .compile("message_container.selector")?,
            message: self.message.compile()?,
            composer: self.composer.compile()?,
            confirmation: self.confirmation.compile()?,
            activity: self.activity.compile()?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CompiledConnector {
    schema: String,
    id: String,
    version: String,
    browser_binding: String,
    start_url: Url,
    origin_policy: OriginPolicy,
    profile: Profile,
    conversation_list: SelectorBinding,
    message_container: SelectorBinding,
    message: MessageBinding,
    composer: Composer,
    confirmation: ConfirmationPolicy,
    activity: ActivityPolicy,
}

impl CompiledConnector {
    pub fn schema(&self) -> &str {
        &self.schema
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn browser_binding(&self) -> &str {
        &self.browser_binding
    }
    pub fn start_url(&self) -> &Url {
        &self.start_url
    }
    pub fn origin_policy(&self) -> &OriginPolicy {
        &self.origin_policy
    }
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    pub fn conversation_list(&self) -> &SelectorBinding {
        &self.conversation_list
    }
    pub fn message_container(&self) -> &SelectorBinding {
        &self.message_container
    }
    pub fn message(&self) -> &MessageBinding {
        &self.message
    }
    pub fn composer(&self) -> &Composer {
        &self.composer
    }
    pub fn confirmation(&self) -> &ConfirmationPolicy {
        &self.confirmation
    }
    pub fn activity(&self) -> &ActivityPolicy {
        &self.activity
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginPolicyDefinition {
    allow: Vec<String>,
    #[serde(default)]
    auth_allow: Vec<String>,
    #[serde(default)]
    deny_private_networks: bool,
}

impl OriginPolicyDefinition {
    fn compile(self) -> Result<OriginPolicy> {
        let allow = compile_urls("origin_policy.allow", self.allow)?;
        let auth_allow = compile_urls("origin_policy.auth_allow", self.auth_allow)?;
        Ok(OriginPolicy {
            allow,
            auth_allow,
            deny_private_networks: self.deny_private_networks,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct OriginPolicy {
    allow: Vec<Url>,
    auth_allow: Vec<Url>,
    deny_private_networks: bool,
}

impl OriginPolicy {
    pub fn allow(&self) -> &[Url] {
        &self.allow
    }
    pub fn auth_allow(&self) -> &[Url] {
        &self.auth_allow
    }
    pub const fn deny_private_networks(&self) -> bool {
        self.deny_private_networks
    }

    fn allows(&self, candidate: &Url) -> bool {
        self.allow
            .iter()
            .any(|allowed| same_origin(allowed, candidate))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileDefinition {
    mode: ProfileMode,
    profile_binding: Option<String>,
}

impl ProfileDefinition {
    fn compile(self) -> Result<Profile> {
        if self.mode == ProfileMode::Persistent
            && self.profile_binding.as_deref().is_none_or(str::is_empty)
        {
            return Err(Error::PersistentProfileBindingMissing);
        }
        Ok(Profile {
            mode: self.mode,
            binding: self.profile_binding,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileMode {
    Ephemeral,
    Persistent,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Profile {
    mode: ProfileMode,
    binding: Option<String>,
}

impl Profile {
    pub const fn mode(&self) -> ProfileMode {
        self.mode
    }
    pub fn binding(&self) -> Option<&str> {
        self.binding.as_deref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectorBlock {
    selector: SelectorSpec,
}

impl SelectorBlock {
    fn compile(self, location: &'static str) -> Result<SelectorBinding> {
        Ok(SelectorBinding {
            selector: self.selector.compile(location)?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SelectorBinding {
    selector: Target,
}

impl SelectorBinding {
    pub fn selector(&self) -> &Target {
        &self.selector
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageDefinition {
    selector: SelectorSpec,
    id_attribute: Option<String>,
    sender: SelectorBlock,
    body: SelectorBlock,
}

impl MessageDefinition {
    fn compile(self) -> Result<MessageBinding> {
        if self.id_attribute.as_deref().is_some_and(str::is_empty) {
            return Err(Error::EmptyField {
                field: "message.id_attribute",
            });
        }
        Ok(MessageBinding {
            selector: self.selector.compile("message.selector")?,
            id_attribute: self.id_attribute,
            sender: self.sender.compile("message.sender.selector")?,
            body: self.body.compile("message.body.selector")?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MessageBinding {
    selector: Target,
    id_attribute: Option<String>,
    sender: SelectorBinding,
    body: SelectorBinding,
}

impl MessageBinding {
    pub fn selector(&self) -> &Target {
        &self.selector
    }
    pub fn id_attribute(&self) -> Option<&str> {
        self.id_attribute.as_deref()
    }
    pub fn sender(&self) -> &SelectorBinding {
        &self.sender
    }
    pub fn body(&self) -> &SelectorBinding {
        &self.body
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposerDefinition {
    input: SelectorSpec,
    send: SelectorSpec,
}

impl ComposerDefinition {
    fn compile(self) -> Result<Composer> {
        Ok(Composer {
            input: self.input.compile("composer.input")?,
            send: self.send.compile("composer.send")?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Composer {
    input: Target,
    send: Target,
}

impl Composer {
    pub fn input(&self) -> &Target {
        &self.input
    }
    pub fn send(&self) -> &Target {
        &self.send
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmationDefinition {
    mode: ConfirmationMode,
    request_path: Option<String>,
    own_message_selector: Option<SelectorSpec>,
}

impl ConfirmationDefinition {
    fn compile(self) -> Result<ConfirmationPolicy> {
        if self
            .request_path
            .as_deref()
            .is_some_and(|path| !path.starts_with('/'))
        {
            return Err(Error::Confirmation {
                issue: ConfirmationIssue::RequestPathMustBeAbsolute,
            });
        }
        match self.mode {
            ConfirmationMode::Network if self.request_path.is_none() => {
                return Err(Error::Confirmation {
                    issue: ConfirmationIssue::MissingNetworkRequestPath,
                });
            }
            ConfirmationMode::Dom if self.own_message_selector.is_none() => {
                return Err(Error::Confirmation {
                    issue: ConfirmationIssue::MissingDomSelector,
                });
            }
            ConfirmationMode::NetworkOrDom
                if self.request_path.is_none() && self.own_message_selector.is_none() =>
            {
                return Err(Error::Confirmation {
                    issue: ConfirmationIssue::MissingNetworkAndDomEvidence,
                });
            }
            ConfirmationMode::Network | ConfirmationMode::Dom | ConfirmationMode::NetworkOrDom => {}
        }
        let own_message_selector = self
            .own_message_selector
            .map(|selector| selector.compile("confirmation.own_message_selector"))
            .transpose()?;
        Ok(ConfirmationPolicy {
            mode: self.mode,
            request_path: self.request_path,
            own_message_selector,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfirmationMode {
    Network,
    Dom,
    NetworkOrDom,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConfirmationPolicy {
    mode: ConfirmationMode,
    request_path: Option<String>,
    own_message_selector: Option<Target>,
}

impl ConfirmationPolicy {
    pub const fn mode(&self) -> ConfirmationMode {
        self.mode
    }
    pub fn request_path(&self) -> Option<&str> {
        self.request_path.as_deref()
    }
    pub fn own_message_selector(&self) -> Option<&Target> {
        self.own_message_selector.as_ref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityDefinition {
    human_takeover_selector: SelectorSpec,
}

impl ActivityDefinition {
    fn compile(self) -> Result<ActivityPolicy> {
        Ok(ActivityPolicy {
            human_takeover_selector: self
                .human_takeover_selector
                .compile("activity.human_takeover_selector")?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ActivityPolicy {
    human_takeover_selector: Target,
}

impl ActivityPolicy {
    pub fn human_takeover_selector(&self) -> &Target {
        &self.human_takeover_selector
    }
}

#[derive(Debug)]
struct SelectorSpec {
    primary: SelectorValue,
    fallbacks: Vec<SelectorSpec>,
}

impl SelectorSpec {
    fn compile(self, location: &'static str) -> Result<Target> {
        let primary = self.primary.compile(location)?;
        let mut fallbacks = Vec::with_capacity(self.fallbacks.len());
        for fallback in self.fallbacks {
            if !fallback.fallbacks.is_empty() {
                return Err(Error::Selector {
                    location,
                    issue: SelectorIssue::EmptyFallback,
                });
            }
            fallbacks.push(fallback.primary.compile(location)?);
        }
        Ok(Target { primary, fallbacks })
    }
}

impl<'de> Deserialize<'de> for SelectorSpec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SelectorWire::deserialize(deserializer)?;
        let strategies = wire.strategies();
        if strategies.len() != 1 {
            return Err(D::Error::custom(if strategies.is_empty() {
                "selector must specify exactly one strategy".to_owned()
            } else {
                format!(
                    "selector strategies `{}` and `{}` are ambiguous",
                    strategies[0], strategies[1]
                )
            }));
        }
        let (primary, fallbacks) = wire.into_value(strategies[0]).map_err(D::Error::custom)?;
        Ok(Self { primary, fallbacks })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectorWire {
    test_id: Option<String>,
    css: Option<String>,
    id: Option<String>,
    name: Option<String>,
    role: Option<String>,
    xpath: Option<String>,
    link_text: Option<String>,
    text_anchor: Option<String>,
    tag: Option<String>,
    class: Option<String>,
    #[serde(default)]
    fallbacks: Vec<SelectorSpec>,
}

impl SelectorWire {
    fn strategies(&self) -> Vec<SelectorStrategy> {
        let mut values = Vec::new();
        if self.test_id.is_some() {
            values.push(SelectorStrategy::TestId);
        }
        if self.css.is_some() {
            values.push(SelectorStrategy::Css);
        }
        if self.id.is_some() {
            values.push(SelectorStrategy::Id);
        }
        if self.role.is_some() {
            values.push(SelectorStrategy::Role);
        }
        if self.xpath.is_some() {
            values.push(SelectorStrategy::XPath);
        }
        if self.link_text.is_some() {
            values.push(SelectorStrategy::LinkText);
        }
        if self.text_anchor.is_some() {
            values.push(SelectorStrategy::TextAnchor);
        }
        if self.tag.is_some() || self.class.is_some() {
            values.push(SelectorStrategy::TagClass);
        }
        if self.name.is_some() && self.role.is_none() {
            values.push(SelectorStrategy::Name);
        }
        values
    }

    fn into_value(
        self,
        strategy: SelectorStrategy,
    ) -> std::result::Result<(SelectorValue, Vec<SelectorSpec>), &'static str> {
        let selector = match strategy {
            SelectorStrategy::TestId => Selector::TestId(self.test_id.unwrap_or_default()),
            SelectorStrategy::Css => Selector::Css(self.css.unwrap_or_default()),
            SelectorStrategy::Id => Selector::Id(self.id.unwrap_or_default()),
            SelectorStrategy::Name => Selector::Name(self.name.unwrap_or_default()),
            SelectorStrategy::Role => Selector::Role {
                role: self.role.unwrap_or_default(),
                name: self.name,
            },
            SelectorStrategy::XPath => Selector::XPath(self.xpath.unwrap_or_default()),
            SelectorStrategy::LinkText => Selector::LinkText(self.link_text.unwrap_or_default()),
            SelectorStrategy::TextAnchor => {
                Selector::TextAnchor(self.text_anchor.unwrap_or_default())
            }
            SelectorStrategy::TagClass => Selector::TagClass {
                tag: self.tag.ok_or("tag_class selector requires `tag`")?,
                class: self.class.ok_or("tag_class selector requires `class`")?,
            },
        };
        Ok((SelectorValue { selector, strategy }, self.fallbacks))
    }
}

#[derive(Debug)]
struct SelectorValue {
    selector: Selector,
    strategy: SelectorStrategy,
}

impl SelectorValue {
    fn compile(self, location: &'static str) -> Result<Selector> {
        let empty = match &self.selector {
            Selector::TestId(value)
            | Selector::Css(value)
            | Selector::Id(value)
            | Selector::Name(value)
            | Selector::LinkText(value)
            | Selector::XPath(value)
            | Selector::TextAnchor(value) => value.is_empty(),
            Selector::Role { role, .. } => role.is_empty(),
            Selector::TagClass { tag, class } => tag.is_empty() || class.is_empty(),
        };
        if empty {
            return Err(Error::Selector {
                location,
                issue: SelectorIssue::EmptyValue {
                    strategy: self.strategy,
                },
            });
        }
        Ok(self.selector)
    }
}

fn require_non_empty(value: &str, field: &'static str) -> Result<()> {
    if value.is_empty() {
        Err(Error::EmptyField { field })
    } else {
        Ok(())
    }
}

fn parse_url(field: &'static str, value: String) -> Result<Url> {
    Url::parse(&value).map_err(|source| Error::InvalidUrl {
        field,
        value,
        source,
    })
}

fn compile_urls(field: &'static str, values: Vec<String>) -> Result<Vec<Url>> {
    values
        .into_iter()
        .map(|value| parse_url(field, value))
        .collect()
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}
