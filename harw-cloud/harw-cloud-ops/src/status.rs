//! Profile-bound cloud diagnostics. Filesystem observations are not service
//! identity, validated configuration, or authenticated readiness evidence.

use std::path::Path;
use std::sync::Arc;

use harw_home::ResolvedHomeContext;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use serde::Deserialize;
use serde_json::json;

/// Arguments of `cloud.status` (none).
#[derive(Debug, Default, Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusArgs {}

fn directory_observation(path: &Path) -> &'static str {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => "present",
        Ok(_) => "not_directory",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        Err(_) => "unknown",
    }
}

fn status_json(home: &ResolvedHomeContext) -> serde_json::Value {
    json!({
        "schema_version": 2,
        "profile": home.profile_name,
        "profile_directory": directory_observation(&home.profile_dir),
        "installed": null,
        "configured": null,
        "running": null,
        "ready": null,
        "evidence": {
            "source": "bound_profile_filesystem",
            "installed": "unknown: no profile-bound service installation evidence",
            "configured": "unknown: profile presence does not validate service configuration",
            "running": "unknown: no profile-bound service identity observation",
            "ready": "unknown: no authenticated operation-specific readiness observation"
        }
    })
}

/// Inspect the bound profile without starting a service or probing guessed ports.
///
/// # Errors
/// [`OpError::NotAvailable`] if trusted caller or profile context is absent.
#[operation(
    name = "cloud.status",
    summary = "Inspect the selected cloud profile and report service readiness evidence; unknown states remain explicit.",
    domain = "execution",
    permission = "maintainer",
    model_tool(readonly, approval = "none")
)]
async fn cloud_status(ctx: &OpContext, _args: CloudStatusArgs) -> Result<OpOutput, OpError> {
    let local_operator = ctx
        .service::<crate::context::LocalProfileOperator>()
        .is_some();
    if ctx.tenant().is_none() && !local_operator {
        return Err(OpError::NotAvailable(
            "cloud status requires a caller tenant or explicit local-profile operator capability"
                .into(),
        ));
    }
    let home = ctx.service::<Arc<ResolvedHomeContext>>().ok_or_else(|| {
        OpError::NotAvailable("cloud status requires a bound home/profile context".into())
    })?;
    let mut data = status_json(home);
    data["visibility"] = json!(if ctx.tenant().is_some() {
        "tenant"
    } else {
        "local_profile_operator"
    });
    Ok(OpOutput {
        text: format!(
            "Cloud profile {}: directory {}; installation, configuration, running state and readiness unknown (service evidence not bound)",
            home.profile_name,
            directory_observation(&home.profile_dir)
        ),
        data: Some(data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_home::{ProjectKind, ProjectRoot};

    #[test]
    fn existing_profile_does_not_prove_service_readiness() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let project = ProjectRoot {
            root: dir.path().to_owned(),
            trust_key: dir.path().to_owned(),
            kind: ProjectKind::Directory,
        };
        let home = ResolvedHomeContext::new(dir.path(), "selected".into(), project)?;
        std::fs::create_dir_all(&home.profile_dir)?;
        let data = status_json(&home);
        assert_eq!(data["profile"], "selected");
        assert_eq!(data["profile_directory"], "present");
        for state in ["installed", "configured", "running", "ready"] {
            assert!(
                data[state].is_null(),
                "{state} must not be inferred from a directory"
            );
        }
        assert!(data.get("ports").is_none());
        Ok(())
    }

    #[test]
    fn missing_invalid_and_uncheckable_paths_are_distinct() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("file");
        std::fs::write(&file, "not a directory")?;
        assert_eq!(directory_observation(dir.path()), "present");
        assert_eq!(directory_observation(&file), "not_directory");
        assert_eq!(
            directory_observation(&dir.path().join("missing")),
            "missing"
        );
        assert_eq!(directory_observation(&file.join("child")), "unknown");
        Ok(())
    }
}
