//! The immutable identity of a created container.
//!
//! A container name can be reused: another client of the same engine can
//! remove the verified container and create a different one under the same
//! name between `inspect` and `start`, and the plan would then start a
//! container nobody verified. Everything after `create` therefore acts on the
//! ID that `create` printed, never on the name.

use crate::error::ContainerPolicyError;

/// The full 64-character lowercase hexadecimal ID of a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerId(String);

impl ContainerId {
    /// Parses what `podman create` printed (one ID, trailing newline
    /// allowed). A short ID, a name or any other text is refused.
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidContainerId`].
    pub fn parse(create_stdout: &str) -> Result<Self, ContainerPolicyError> {
        let id = create_stdout.trim();
        let ok = id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if ok {
            Ok(Self(id.to_owned()))
        } else {
            Err(ContainerPolicyError::InvalidContainerId(
                "expected the full 64-character hex id printed by create",
            ))
        }
    }

    /// The ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn only_a_full_lowercase_hex_id_is_accepted() -> TestResult {
        let id = "0123456789abcdef".repeat(4);
        ensure(
            ContainerId::parse(&format!("{id}\n")).is_ok(),
            "with newline",
        )?;
        ensure(ContainerId::parse(&id)?.as_str() == id, "round trip")?;
        for bad in [
            "",
            "harw-r1",
            &id[..12],
            &id.to_uppercase(),
            &format!("{id}0"),
            &format!("{}x", &id[..63]),
            &format!("{id}\nsecond"),
        ] {
            ensure(ContainerId::parse(bad).is_err(), bad)?;
        }
        Ok(())
    }
}
