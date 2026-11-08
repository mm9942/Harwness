//! Container hardening facts, read — never acted on.
//!
//! [`ContainerSensor`] looks at the container scopes below one cgroup-v2
//! directory (`libpod-<id>.scope`, `docker-<id>.scope`, `crio-<id>.scope`)
//! and, for scopes whose id is in the **expected** set, reads the first
//! process of the scope from `cgroup.procs` and its `status` file from
//! procfs: effective capabilities, `Seccomp` mode and `NoNewPrivs`.
//!
//! # Trust
//! A cgroup name is not proof of ownership. Only ids listed in the
//! configured expected set are inspected; every other scope is reported as
//! `container_unowned` (value 1) and nothing more is read. The sensor has
//! no way to signal, stop or remove anything.
//!
//! # Bounds
//! At most [`MAX_SCOPES`] scopes are examined (sorted by name; a cut is
//! flagged by `container_scopes_truncated`). One pid per scope is read.
//! A scope without a readable pid is `container_unverifiable`, never
//! "fine".
//!
//! # Availability (PL-100 WP1.8)
//! What a missing scope directory (`cgroup_root`) means is the caller's
//! choice ([`MissingRoot`]): `SensorError::SourceUnavailable` (permanent:
//! the sentinel reports the sensor as degraded) for a directory that must
//! exist, or an empty reading for one the runtime creates on demand
//! (systemd creates `machine.slice` only with the first machine or
//! container scope). An existing directory without any container scope is
//! always an empty reading, not an error: a host with a runtime but no
//! running container is not blind, and a permanent error there would
//! retire the sensor before the first container starts.
//!
//! # Not covered
//! Whether the container shares the host network namespace needs
//! `readlink` on `/proc/<pid>/ns/net`, which the read seam does not offer;
//! the executor's inspect read-back covers `NetworkMode` instead.
//!
//! # Metrics
//! All names end in `_<first 12 hex of the id>`:
//! `container_cap_eff_bits`, `container_privileged` (1 when at least
//! [`PRIVILEGED_CAP_BITS`] capabilities are effective), `container_seccomp_mode`
//! (0 off, 1 strict, 2 filter), `container_no_new_privs`,
//! `container_unowned`, `container_unverifiable`.

#![forbid(unsafe_code)]

mod sensor;

pub use sensor::{
    ContainerSensor, ContainerSensorConfig, MAX_CARDINALITY, MAX_SCOPES, MissingRoot,
    PRIVILEGED_CAP_BITS,
};

#[cfg(test)]
mod test_support;
