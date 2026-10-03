//! Maps the engine's inspect read-back to a per-dimension report.
//!
//! The report describes what the engine says it applied, not what was
//! asked: a field the engine does not report is never `Enforced`.

use harw_job_core::{EnforcementState, SandboxReport};

use crate::engine::InspectDoc;

/// What the job requested, as far as the report needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requested {
    /// The network the profile asks for.
    pub network: NetworkIntent,
    /// Requested memory ceiling in bytes.
    pub memory: Option<u64>,
    /// Requested CPU shares.
    pub cpu_shares: Option<u64>,
    /// Requested pids ceiling.
    pub pids: Option<u32>,
}

/// Network intent of a sandbox profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkIntent {
    /// No network at all.
    None,
    /// Network allowed (never the host's namespace).
    Allowed,
    /// Allowlisted network; this backend cannot filter, so `none` is the
    /// best it can do and is reported as partial.
    Restricted,
}

fn flag(value: Option<bool>, wanted: bool) -> EnforcementState {
    match value {
        Some(v) if v == wanted => EnforcementState::Enforced,
        Some(_) => EnforcementState::NotEnforced,
        None => EnforcementState::Partial,
    }
}

fn filesystem(doc: &InspectDoc) -> EnforcementState {
    if doc.host_config.privileged == Some(true) {
        return EnforcementState::NotEnforced;
    }
    let readonly = flag(doc.host_config.readonly_rootfs, true);
    if readonly != EnforcementState::Enforced {
        return readonly;
    }
    let binds_clean = doc.host_config.binds.as_ref().is_none_or(Vec::is_empty);
    match &doc.mounts {
        // Only tmpfs scratch may be attached; anything else (bind, anonymous
        // image volume) is writable storage outside the sandbox contract.
        Some(mounts) if binds_clean && mounts.iter().all(|m| m.r#type == "tmpfs") => {
            EnforcementState::Enforced
        }
        Some(_) => EnforcementState::NotEnforced,
        None => EnforcementState::Partial,
    }
}

fn network(doc: &InspectDoc, want: NetworkIntent) -> EnforcementState {
    let Some(mode) = doc.host_config.network_mode.as_deref() else {
        return EnforcementState::Partial;
    };
    let shares_host = mode == "host" || mode.starts_with("container:");
    match want {
        NetworkIntent::None => {
            if mode == "none" {
                EnforcementState::Enforced
            } else {
                EnforcementState::NotEnforced
            }
        }
        NetworkIntent::Allowed => {
            if shares_host {
                EnforcementState::NotEnforced
            } else {
                EnforcementState::Enforced
            }
        }
        NetworkIntent::Restricted => {
            if mode == "none" {
                EnforcementState::Partial
            } else {
                EnforcementState::NotEnforced
            }
        }
    }
}

fn no_new_privs(doc: &InspectDoc) -> EnforcementState {
    match &doc.host_config.security_opt {
        Some(opts)
            if opts
                .iter()
                .any(|o| matches!(o.as_str(), "no-new-privileges" | "no-new-privileges:true")) =>
        {
            EnforcementState::Enforced
        }
        _ => EnforcementState::NotEnforced,
    }
}

fn capabilities(doc: &InspectDoc) -> EnforcementState {
    let host = &doc.host_config;
    if host.privileged != Some(false) && host.privileged.is_some() {
        return EnforcementState::NotEnforced;
    }
    let dropped_all = host
        .cap_drop
        .as_ref()
        .is_some_and(|drop| drop.iter().any(|c| c.eq_ignore_ascii_case("ALL")));
    let added = host.cap_add.as_ref().is_some_and(|add| !add.is_empty());
    if !dropped_all || added {
        return EnforcementState::NotEnforced;
    }
    if host.privileged.is_none() {
        // Privileged was not reported: the drop is real but the engine did
        // not confirm the container is unprivileged.
        return EnforcementState::Partial;
    }
    EnforcementState::Enforced
}

fn resource_limits(doc: &InspectDoc, want: &Requested) -> EnforcementState {
    let host = &doc.host_config;
    let checks = [
        want.memory.map(|m| host.memory == Some(m)),
        want.cpu_shares.map(|s| host.cpu_shares == Some(s)),
        want.pids.map(|p| host.pids_limit == Some(i64::from(p))),
    ];
    let asked: Vec<bool> = checks.into_iter().flatten().collect();
    if asked.is_empty() || asked.iter().all(|ok| *ok) {
        EnforcementState::Enforced
    } else if asked.iter().any(|ok| *ok) {
        EnforcementState::Partial
    } else {
        EnforcementState::NotEnforced
    }
}

/// The per-dimension report for an inspect document.
#[must_use]
pub fn report_from_inspect(doc: &InspectDoc, want: &Requested) -> SandboxReport {
    SandboxReport {
        filesystem: filesystem(doc),
        network: network(doc, want.network),
        no_new_privs: no_new_privs(doc),
        capabilities: capabilities(doc),
        resource_limits: resource_limits(doc, want),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{HostConfigDoc, MountDoc};

    fn hardened() -> InspectDoc {
        InspectDoc {
            host_config: HostConfigDoc {
                network_mode: Some("none".into()),
                readonly_rootfs: Some(true),
                privileged: Some(false),
                cap_drop: Some(vec!["ALL".into()]),
                cap_add: Some(Vec::new()),
                security_opt: Some(vec!["no-new-privileges".into()]),
                memory: Some(1 << 28),
                pids_limit: Some(64),
                cpu_shares: Some(1024),
                binds: None,
            },
            mounts: Some(vec![MountDoc {
                r#type: "tmpfs".into(),
                destination: "/work".into(),
            }]),
            ..InspectDoc::default()
        }
    }

    fn want() -> Requested {
        Requested {
            network: NetworkIntent::None,
            memory: Some(1 << 28),
            cpu_shares: Some(1024),
            pids: Some(64),
        }
    }

    #[test]
    fn hardened_container_is_fully_enforced() {
        let report = report_from_inspect(&hardened(), &want());
        assert_eq!(report, SandboxReport::uniform(EnforcementState::Enforced));
    }

    #[test]
    fn a_dropped_memory_limit_is_partial() {
        let mut doc = hardened();
        doc.host_config.memory = Some(0);
        let report = report_from_inspect(&doc, &want());
        assert_eq!(report.resource_limits, EnforcementState::Partial);
        assert_eq!(report.filesystem, EnforcementState::Enforced);
    }

    #[test]
    fn missing_fields_are_never_enforced() {
        let report = report_from_inspect(&InspectDoc::default(), &want());
        for (name, state) in report.dimensions() {
            assert_ne!(state, EnforcementState::Enforced, "{name}");
        }
    }

    #[test]
    fn privileged_or_added_caps_or_binds_break_the_dimensions() {
        let mut doc = hardened();
        doc.host_config.privileged = Some(true);
        let report = report_from_inspect(&doc, &want());
        assert_eq!(report.capabilities, EnforcementState::NotEnforced);
        assert_eq!(report.filesystem, EnforcementState::NotEnforced);

        let mut doc = hardened();
        doc.host_config.cap_add = Some(vec!["SYS_ADMIN".into()]);
        assert_eq!(
            report_from_inspect(&doc, &want()).capabilities,
            EnforcementState::NotEnforced
        );

        let mut doc = hardened();
        doc.mounts = Some(vec![MountDoc {
            r#type: "bind".into(),
            destination: "/host".into(),
        }]);
        assert_eq!(
            report_from_inspect(&doc, &want()).filesystem,
            EnforcementState::NotEnforced
        );
    }

    #[test]
    fn network_follows_the_profile_intent() {
        let mut doc = hardened();
        doc.host_config.network_mode = Some("host".into());
        let mut w = want();
        assert_eq!(network(&doc, w.network), EnforcementState::NotEnforced);
        w.network = NetworkIntent::Allowed;
        assert_eq!(network(&doc, w.network), EnforcementState::NotEnforced);
        doc.host_config.network_mode = Some("bridge".into());
        assert_eq!(network(&doc, w.network), EnforcementState::Enforced);
        doc.host_config.network_mode = Some("none".into());
        assert_eq!(
            network(&doc, NetworkIntent::Restricted),
            EnforcementState::Partial
        );
    }
}
