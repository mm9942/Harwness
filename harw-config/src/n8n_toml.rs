//! `[n8n]` — optional outbound integration configuration.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct N8nSection {
    pub enabled: bool,
    pub endpoint_url: Option<String>,
    #[serde(alias = "credential_profile")]
    pub credential_profile_ref: Option<String>,
    pub session_binding: bool,
    pub retry_max_attempts: u32,
    pub retry_initial_backoff_secs: u64,
    pub retry_max_backoff_secs: u64,
    pub dedup_window_secs: u64,
}

impl Default for N8nSection {
    fn default() -> Self {
        Self { enabled: false, endpoint_url: None, credential_profile_ref: None, session_binding: true,
            retry_max_attempts: 3, retry_initial_backoff_secs: 1, retry_max_backoff_secs: 30, dedup_window_secs: 300 }
    }
}

impl N8nSection {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled { return Ok(()); }
        let endpoint = self.endpoint_url.as_deref().ok_or_else(|| "n8n.endpoint_url is required when enabled".to_owned())?;
        let rest = endpoint.strip_prefix("https://").ok_or_else(|| "n8n.endpoint_url must use https".to_owned())?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace) {
            return Err("n8n.endpoint_url must be a valid HTTPS URL without userinfo".to_owned());
        }
        if self.retry_initial_backoff_secs > self.retry_max_backoff_secs {
            return Err("n8n.retry_initial_backoff_secs must not exceed retry_max_backoff_secs".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn defaults_are_fail_closed() {
        let s: N8nSection = toml::from_str("").unwrap();
        assert!(!s.enabled); assert!(s.session_binding); assert_eq!(s, N8nSection::default());
        assert!(s.validate().is_ok());
    }
    #[test] fn enabled_requires_https_endpoint() {
        let s = N8nSection { enabled: true, endpoint_url: Some("http://example.com".into()), ..Default::default() };
        assert!(s.validate().unwrap_err().contains("https"));
    }
    #[test] fn enabled_https_is_valid() {
        let s = N8nSection { enabled: true, endpoint_url: Some("https://example.com/hook".into()), ..Default::default() };
        assert!(s.validate().is_ok());
    }
    #[test] fn token_fields_are_rejected() {
        assert!(toml::from_str::<N8nSection>("enabled = true\ntoken = 'secret'").is_err());
    }
}
