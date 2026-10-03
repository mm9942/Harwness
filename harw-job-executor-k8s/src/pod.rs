//! The pod manifest and the read-back of the admitted pod.

use std::collections::BTreeMap;

use harw_container_model::OwnerLabels;
use harw_job_core::{EnforcementState, JobSpec, SandboxProfileName, SandboxReport};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::K8sConfig;
use crate::error::K8sError;

/// The scheduling gate that holds a pod until its read-back passed.
pub const GATE: &str = "harw.dev/readback";

/// Network intent of a sandbox profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkIntent {
    /// No network.
    None,
    /// Network allowed.
    Allowed,
    /// Allowlisted network (not enforceable by a pod spec).
    Restricted,
}

/// What the job requested, as far as the report needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requested {
    /// Profile network intent.
    pub network: NetworkIntent,
    /// Requested memory ceiling in bytes.
    pub memory: Option<u64>,
    /// A CPU weight or pids ceiling was requested (neither is expressible
    /// in a pod spec).
    pub inexpressible_limits: bool,
}

pub(crate) fn intent(profile: SandboxProfileName) -> NetworkIntent {
    match profile {
        SandboxProfileName::WorkspaceBuild => NetworkIntent::Allowed,
        SandboxProfileName::ReadOnlyAnalysis | SandboxProfileName::NoNetwork => NetworkIntent::None,
        SandboxProfileName::NetworkRestricted => NetworkIntent::Restricted,
    }
}

fn fnv64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Deterministic pod (and container) name for an attempt.
pub(crate) fn pod_name(job_id: &str, epoch: u64) -> String {
    format!("harw-{:016x}-e{epoch}", fnv64(job_id))
}

/// Labels as Kubernetes accepts them (no `:`, at most 63 characters).
pub(crate) fn k8s_labels(labels: &OwnerLabels) -> Result<BTreeMap<String, String>, K8sError> {
    let map = labels.to_map();
    for (key, value) in &map {
        if value.len() > 63
            || value.contains(':')
            || value.starts_with(['-', '.', '_'])
            || value.ends_with(['-', '.', '_'])
        {
            return Err(K8sError::Config(format!(
                "label `{key}` is not a valid Kubernetes label value"
            )));
        }
    }
    Ok(map)
}

fn work_dir(spec: &JobSpec) -> Result<String, K8sError> {
    let rel = spec.working_dir.as_str();
    if rel.contains('\\') {
        return Err(K8sError::Unsupported("backslash in working_dir".into()));
    }
    let rel = rel.trim_matches('/');
    Ok(if rel.is_empty() || rel == "." {
        "/work".to_owned()
    } else {
        format!("/work/{rel}")
    })
}

/// The pod to create: restricted, gated, no service-account token.
pub(crate) fn manifest(
    cfg: &K8sConfig,
    spec: &JobSpec,
    name: &str,
    image_ref: &str,
    labels: &BTreeMap<String, String>,
) -> Result<Value, K8sError> {
    let env: Vec<Value> = spec
        .env
        .iter()
        .map(|(k, v)| json!({"name": k, "value": v}))
        .collect();
    let mut container = json!({
        "name": name,
        "image": image_ref,
        "imagePullPolicy": "IfNotPresent",
        "command": [spec.program],
        "args": spec.args,
        "env": env,
        "workingDir": work_dir(spec)?,
        "securityContext": {
            "allowPrivilegeEscalation": false,
            "readOnlyRootFilesystem": true,
            "privileged": false,
            "capabilities": {"drop": ["ALL"]},
        },
        "volumeMounts": [
            {"name": "work", "mountPath": "/work"},
            {"name": "tmp", "mountPath": "/tmp"},
        ],
    });
    if let Some(memory) = spec.resources.memory_max {
        container["resources"] = json!({"limits": {"memory": memory.to_string()}});
    }
    Ok(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": name, "namespace": cfg.namespace, "labels": labels},
        "spec": {
            "restartPolicy": "Never",
            "automountServiceAccountToken": false,
            "enableServiceLinks": false,
            "hostNetwork": false,
            "hostPID": false,
            "hostIPC": false,
            "schedulingGates": [{"name": GATE}],
            "securityContext": {
                "runAsNonRoot": true,
                "runAsUser": cfg.run_as,
                "runAsGroup": cfg.run_as,
                "seccompProfile": {"type": "RuntimeDefault"},
            },
            "volumes": [
                {"name": "work", "emptyDir": {"sizeLimit": cfg.work_bytes.to_string()}},
                {"name": "tmp", "emptyDir": {"medium": "Memory", "sizeLimit": "67108864"}},
            ],
            "containers": [container],
        },
    }))
}

/// The parts of a pod the executor reads back (all optional: a field the
/// API did not report is never treated as enforced).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PodDoc {
    /// Metadata.
    pub metadata: Meta,
    /// Spec as admitted.
    pub spec: Spec,
    /// Status.
    pub status: Status,
}

/// Pod metadata.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Meta {
    /// Pod name.
    pub name: String,
    /// Immutable pod UID.
    pub uid: String,
    /// Labels.
    pub labels: BTreeMap<String, String>,
}

/// Pod spec.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Spec {
    /// Containers.
    pub containers: Vec<ContainerSpec>,
    /// Volumes (raw: only the kind matters).
    pub volumes: Vec<Value>,
    /// Host network.
    pub host_network: Option<bool>,
    /// Host PID namespace.
    pub host_pid: Option<bool>,
    /// Host IPC namespace.
    pub host_ipc: Option<bool>,
    /// Service-account token automount.
    pub automount_service_account_token: Option<bool>,
    /// Pod-level security context.
    pub security_context: PodSecurity,
    /// Remaining scheduling gates.
    pub scheduling_gates: Vec<Value>,
}

/// Pod-level security context.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PodSecurity {
    /// Must not run as root.
    pub run_as_non_root: Option<bool>,
}

/// A container of the pod.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContainerSpec {
    /// Container name.
    pub name: String,
    /// Image reference.
    pub image: String,
    /// Container security context.
    pub security_context: ContainerSecurity,
    /// Resources.
    pub resources: Resources,
}

/// Container security context.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContainerSecurity {
    /// Privilege escalation.
    pub allow_privilege_escalation: Option<bool>,
    /// Read-only root filesystem.
    pub read_only_root_filesystem: Option<bool>,
    /// Privileged mode.
    pub privileged: Option<bool>,
    /// Capabilities.
    pub capabilities: Option<Caps>,
}

/// Capability lists.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Caps {
    /// Added.
    pub add: Option<Vec<String>>,
    /// Dropped.
    pub drop: Option<Vec<String>>,
}

/// Resources.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Resources {
    /// Limits by resource name.
    pub limits: Option<BTreeMap<String, String>>,
}

/// Pod status.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    /// `Pending`, `Running`, `Succeeded`, `Failed`, `Unknown`.
    pub phase: Option<String>,
    /// Per-container status.
    pub container_statuses: Vec<ContainerStatus>,
}

/// Container status.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContainerStatus {
    /// State.
    pub state: State,
}

/// Container state.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct State {
    /// Set once terminated.
    pub terminated: Option<Terminated>,
}

/// Termination details.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Terminated {
    /// Exit code.
    pub exit_code: Option<i64>,
}

impl PodDoc {
    /// Whether the pod reached a final phase.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.phase.as_deref(), Some("Succeeded" | "Failed"))
    }

    /// The exit code of the (only) container, once terminated.
    #[must_use]
    pub fn exit_code(&self) -> Option<i64> {
        self.status
            .container_statuses
            .first()
            .and_then(|c| c.state.terminated.as_ref())
            .and_then(|t| t.exit_code)
    }
}

/// Parses a Kubernetes quantity into bytes (plain integer or binary/decimal suffix).
pub(crate) fn quantity_bytes(text: &str) -> Option<u64> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, suffix) = text.split_at(split);
    let base: u64 = digits.parse().ok()?;
    let factor: u64 = match suffix {
        "" => 1,
        "k" => 1_000,
        "M" => 1_000_000,
        "G" => 1_000_000_000,
        "Ki" => 1 << 10,
        "Mi" => 1 << 20,
        "Gi" => 1 << 30,
        "Ti" => 1 << 40,
        _ => return None,
    };
    base.checked_mul(factor)
}

fn container(doc: &PodDoc) -> Option<&ContainerSpec> {
    // Exactly one container: a webhook-injected sidecar is not what was asked.
    match doc.spec.containers.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

fn filesystem(doc: &PodDoc) -> EnforcementState {
    let Some(c) = container(doc) else {
        return EnforcementState::Partial;
    };
    let host_shared = doc.spec.host_pid == Some(true) || doc.spec.host_ipc == Some(true);
    let volumes_clean = doc.spec.volumes.iter().all(|v| v.get("emptyDir").is_some());
    match c.security_context.read_only_root_filesystem {
        _ if host_shared || !volumes_clean => EnforcementState::NotEnforced,
        Some(true) => EnforcementState::Enforced,
        Some(false) => EnforcementState::NotEnforced,
        None => EnforcementState::Partial,
    }
}

fn network(doc: &PodDoc, want: &Requested, attested: bool) -> EnforcementState {
    if doc.spec.host_network == Some(true) {
        return EnforcementState::NotEnforced;
    }
    match want.network {
        NetworkIntent::Allowed => match doc.spec.host_network {
            Some(false) => EnforcementState::Enforced,
            _ => EnforcementState::Partial,
        },
        NetworkIntent::None if attested && doc.spec.host_network == Some(false) => {
            EnforcementState::Enforced
        }
        // A pod spec cannot deny egress; without an operator attestation of
        // the namespace's NetworkPolicy the best honest answer is Partial.
        NetworkIntent::None | NetworkIntent::Restricted => EnforcementState::Partial,
    }
}

fn no_new_privs(doc: &PodDoc) -> EnforcementState {
    match container(doc).and_then(|c| c.security_context.allow_privilege_escalation) {
        Some(false) => EnforcementState::Enforced,
        Some(true) => EnforcementState::NotEnforced,
        None => EnforcementState::Partial,
    }
}

fn capabilities(doc: &PodDoc) -> EnforcementState {
    let Some(c) = container(doc) else {
        return EnforcementState::Partial;
    };
    let sec = &c.security_context;
    let dropped_all = sec
        .capabilities
        .as_ref()
        .and_then(|caps| caps.drop.as_ref())
        .is_some_and(|d| d.iter().any(|x| x.eq_ignore_ascii_case("ALL")));
    let added = sec
        .capabilities
        .as_ref()
        .and_then(|caps| caps.add.as_ref())
        .is_some_and(|a| !a.is_empty());
    if sec.privileged == Some(true) || added || !dropped_all {
        return EnforcementState::NotEnforced;
    }
    if doc.spec.security_context.run_as_non_root == Some(true) {
        EnforcementState::Enforced
    } else {
        EnforcementState::Partial
    }
}

fn resource_limits(doc: &PodDoc, want: &Requested) -> EnforcementState {
    let memory_ok = want.memory.map(|wanted| {
        container(doc)
            .and_then(|c| c.resources.limits.as_ref())
            .and_then(|l| l.get("memory"))
            .and_then(|q| quantity_bytes(q))
            == Some(wanted)
    });
    match (memory_ok, want.inexpressible_limits) {
        (None, false) => EnforcementState::Enforced,
        (Some(true), false) => EnforcementState::Enforced,
        (Some(false), _) => EnforcementState::NotEnforced,
        // A CPU weight or pids ceiling cannot be expressed per pod.
        (_, true) => EnforcementState::Partial,
    }
}

/// The per-dimension report for an admitted pod.
#[must_use]
pub fn report_from_pod(doc: &PodDoc, want: &Requested, network_attested: bool) -> SandboxReport {
    SandboxReport {
        filesystem: filesystem(doc),
        network: network(doc, want, network_attested),
        no_new_privs: no_new_privs(doc),
        capabilities: capabilities(doc),
        resource_limits: resource_limits(doc, want),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hardened() -> PodDoc {
        PodDoc {
            spec: Spec {
                containers: vec![ContainerSpec {
                    security_context: ContainerSecurity {
                        allow_privilege_escalation: Some(false),
                        read_only_root_filesystem: Some(true),
                        privileged: Some(false),
                        capabilities: Some(Caps {
                            add: None,
                            drop: Some(vec!["ALL".into()]),
                        }),
                    },
                    resources: Resources {
                        limits: Some(BTreeMap::from([("memory".into(), "256Mi".into())])),
                    },
                    ..ContainerSpec::default()
                }],
                volumes: vec![json!({"name": "w", "emptyDir": {}})],
                host_network: Some(false),
                security_context: PodSecurity {
                    run_as_non_root: Some(true),
                },
                ..Spec::default()
            },
            ..PodDoc::default()
        }
    }

    fn want() -> Requested {
        Requested {
            network: NetworkIntent::None,
            memory: Some(256 << 20),
            inexpressible_limits: false,
        }
    }

    #[test]
    fn quantities_parse() {
        assert_eq!(quantity_bytes("268435456"), Some(268_435_456));
        assert_eq!(quantity_bytes("256Mi"), Some(268_435_456));
        assert_eq!(quantity_bytes("1G"), Some(1_000_000_000));
        assert_eq!(quantity_bytes("1.5Gi"), None);
        assert_eq!(quantity_bytes("12x"), None);
    }

    #[test]
    fn network_is_partial_without_an_attestation() {
        let doc = hardened();
        let plain = report_from_pod(&doc, &want(), false);
        assert_eq!(plain.network, EnforcementState::Partial);
        assert_eq!(plain.filesystem, EnforcementState::Enforced);
        let attested = report_from_pod(&doc, &want(), true);
        assert_eq!(attested, SandboxReport::uniform(EnforcementState::Enforced));
    }

    #[test]
    fn injected_sidecars_hostpath_and_privilege_break_the_report() {
        let mut doc = hardened();
        doc.spec.containers.push(ContainerSpec::default());
        assert_eq!(
            report_from_pod(&doc, &want(), true).capabilities,
            EnforcementState::Partial
        );
        let mut doc = hardened();
        doc.spec
            .volumes
            .push(json!({"name": "h", "hostPath": {"path": "/"}}));
        assert_eq!(
            report_from_pod(&doc, &want(), true).filesystem,
            EnforcementState::NotEnforced
        );
        let mut doc = hardened();
        doc.spec.containers[0].security_context.privileged = Some(true);
        assert_eq!(
            report_from_pod(&doc, &want(), true).capabilities,
            EnforcementState::NotEnforced
        );
        let mut doc = hardened();
        doc.spec.host_network = Some(true);
        assert_eq!(
            report_from_pod(&doc, &want(), true).network,
            EnforcementState::NotEnforced
        );
    }

    #[test]
    fn missing_fields_are_never_enforced_and_limits_must_match() {
        let report = report_from_pod(&PodDoc::default(), &want(), true);
        for (name, state) in report.dimensions() {
            assert_ne!(state, EnforcementState::Enforced, "{name}");
        }
        let mut doc = hardened();
        doc.spec.containers[0].resources.limits = None;
        assert_eq!(
            report_from_pod(&doc, &want(), true).resource_limits,
            EnforcementState::NotEnforced
        );
        let cpu = Requested {
            inexpressible_limits: true,
            ..want()
        };
        assert_eq!(
            report_from_pod(&hardened(), &cpu, true).resource_limits,
            EnforcementState::Partial
        );
    }

    #[test]
    fn pod_names_are_stable_dns_labels_and_labels_are_checked() {
        let name = pod_name("job-1", 7);
        assert_eq!(name, pod_name("job-1", 7));
        assert_ne!(name, pod_name("job-2", 7));
        assert!(crate::config::is_dns_label(&name));
        let ok = OwnerLabels::new("r", "job-1", 1, 1, "t", "no_network");
        assert!(ok.is_ok_and(|l| k8s_labels(&l).is_ok()));
        let bad = OwnerLabels::new("r", "job:1", 1, 1, "t", "p");
        assert!(bad.is_ok_and(|l| k8s_labels(&l).is_err()));
    }
}
