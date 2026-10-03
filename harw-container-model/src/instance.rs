//! Persistable identities of managed container instances.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::digest::ImageDigest;

/// Which engine runs the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    /// Podman (rootless by default).
    Podman,
    /// Docker.
    Docker,
    /// Kubernetes (pods).
    Kubernetes,
}

impl EngineKind {
    /// Stable lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
            Self::Kubernetes => "kubernetes",
        }
    }
}

/// Why an identity value was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceError {
    /// Not 12 to 64 lowercase hex characters.
    InvalidContainerId,
    /// Not a Kubernetes DNS label (`[a-z0-9-]`, at most 63, alphanumeric ends).
    InvalidNamespace,
    /// Not a UID-like token (`[A-Za-z0-9-]`, 8 to 64 characters).
    InvalidPodUid,
}

impl fmt::Display for InstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidContainerId => "container id must be 12-64 lowercase hex characters",
            Self::InvalidNamespace => "namespace must be a DNS label",
            Self::InvalidPodUid => "pod uid must be 8-64 characters of [A-Za-z0-9-]",
        })
    }
}

impl std::error::Error for InstanceError {}

/// An engine-assigned container id (hex). The only handle acted upon.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContainerId(String);

impl ContainerId {
    /// Validates an id.
    ///
    /// # Errors
    /// [`InstanceError::InvalidContainerId`].
    pub fn new(raw: &str) -> Result<Self, InstanceError> {
        if (12..=64).contains(&raw.len())
            && raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(Self(raw.to_owned()))
        } else {
            Err(InstanceError::InvalidContainerId)
        }
    }

    /// The id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContainerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ContainerId {
    type Error = InstanceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl From<ContainerId> for String {
    fn from(value: ContainerId) -> Self {
        value.0
    }
}

/// A container managed by a local engine (Podman or Docker).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerInstance {
    /// The engine.
    pub engine: EngineKind,
    /// The engine-assigned id.
    pub container_id: ContainerId,
    /// Creation time as Unix seconds (as the engine reported it).
    pub created_unix: i64,
    /// The pinned image the container was created from.
    pub image_digest: ImageDigest,
}

/// A pod (and one container in it) managed through Kubernetes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PodInstance {
    /// Namespace (one per tenant).
    pub namespace: String,
    /// Pod UID (immutable; a recreated pod of the same name has another).
    pub pod_uid: String,
    /// Container name inside the pod.
    pub container: String,
}

impl PodInstance {
    /// Validates the parts.
    ///
    /// # Errors
    /// [`InstanceError`] for a bad namespace or pod uid.
    pub fn new(namespace: &str, pod_uid: &str, container: &str) -> Result<Self, InstanceError> {
        let dns = !namespace.is_empty()
            && namespace.len() <= 63
            && namespace
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !namespace.starts_with('-')
            && !namespace.ends_with('-');
        if !dns {
            return Err(InstanceError::InvalidNamespace);
        }
        if !(8..=64).contains(&pod_uid.len())
            || !pod_uid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(InstanceError::InvalidPodUid);
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            pod_uid: pod_uid.to_owned(),
            container: container.to_owned(),
        })
    }
}

/// Either kind of instance (what a placement or a sensor refers to).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum InstanceRef {
    /// A local engine container.
    Container(ContainerInstance),
    /// A Kubernetes pod.
    Pod(PodInstance),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_ids_are_hex_of_engine_length() {
        assert!(ContainerId::new("0123456789ab").is_ok());
        assert!(ContainerId::new(&"a".repeat(64)).is_ok());
        for bad in [
            "",
            "short",
            "ZZZZZZZZZZZZ",
            "0123456789a",
            &"a".repeat(65),
            "my-container-name",
        ] {
            assert!(ContainerId::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pod_parts_are_validated() {
        assert!(PodInstance::new("tenant-a", "123e4567-e89b", "job").is_ok());
        assert_eq!(
            PodInstance::new("Tenant", "123e4567-e89b", "job"),
            Err(InstanceError::InvalidNamespace)
        );
        assert_eq!(
            PodInstance::new("a", "short", "job"),
            Err(InstanceError::InvalidPodUid)
        );
    }

    #[test]
    fn instances_round_trip_through_json_and_reject_unknown_fields() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let instance = ContainerInstance {
            engine: EngineKind::Podman,
            container_id: ContainerId::new("0123456789ab").unwrap_or_else(|_| unreachable!()),
            created_unix: 1_790_000_000,
            image_digest: ImageDigest::parse(&format!("rust@sha256:{hex}"))
                .unwrap_or_else(|_| unreachable!()),
        };
        let reference = InstanceRef::Container(instance.clone());
        let json = serde_json::to_string(&reference).unwrap_or_default();
        assert!(json.contains("\"kind\":\"container\""), "{json}");
        let back: Result<InstanceRef, _> = serde_json::from_str(&json);
        assert_eq!(back.ok(), Some(reference));
        let extra = json.replace('}', ",\"x\":1}");
        assert!(serde_json::from_str::<InstanceRef>(&extra).is_err());
    }
}
