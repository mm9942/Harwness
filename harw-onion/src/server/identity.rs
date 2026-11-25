//! Persistent identity verification before Arti publishes an Onion descriptor.

use safelog::DisplayRedacted;
use tor_hsservice::OnionServiceConfig;

use super::OnionServerConfig;
use crate::client::runtime::{ArtiClient, client_config, map_arti_error};
use crate::{OnionError, OnionLocator};

pub(super) fn verify_before_launch(
    config: &OnionServerConfig,
    service_config: &OnionServiceConfig,
) -> Result<(), OnionError> {
    let Some(expected) = &config.expected_locator else {
        return Ok(());
    };
    let tor_config = client_config(&config.state_dir, &config.cache_dir)?;
    let unlaunched = ArtiClient::create_onion_service(&tor_config, service_config.clone())
        .map_err(|error| map_arti_error(&error, OnionError::Service))?;
    let identity = unlaunched
        .onion_address()
        .ok_or(OnionError::Identity("stored onion identity is unavailable"))?;
    let actual = OnionLocator::new(
        &identity.display_unredacted().to_string(),
        config.virtual_port,
    )?;
    require_match(expected, &actual)
}

pub(super) fn require_match(
    expected: &OnionLocator,
    actual: &OnionLocator,
) -> Result<(), OnionError> {
    if expected != actual {
        return Err(OnionError::Identity(
            "loaded onion key does not match the stored locator",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_existing_identity_without_bootstrapping() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let mut config = OnionServerConfig::new(dir.path().join("state"), dir.path().join("cache"));
        let tor = client_config(&config.state_dir, &config.cache_dir)?;
        let service = super::super::service_config(&config)?;
        config.expected_locator =
            Some("2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443".parse()?);
        assert!(matches!(
            verify_before_launch(&config, &service),
            Err(OnionError::Identity(_))
        ));
        let unlaunched = ArtiClient::create_onion_service(&tor, service.clone())?;
        let generated = unlaunched.generate_identity_key(Default::default())?;
        assert!(matches!(
            verify_before_launch(&config, &service),
            Err(OnionError::Identity(_))
        ));
        config.expected_locator = Some(OnionLocator::new(
            &generated.display_unredacted().to_string(),
            config.virtual_port,
        )?);
        verify_before_launch(&config, &service)?;
        Ok(())
    }
}
