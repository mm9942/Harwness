use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProfilePolicy {
    Ephemeral,
    Persistent { binding: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BiDiRequirement {
    Required,
    Preferred,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OriginPolicy {
    pub allow: Vec<String>,
    pub deny_private_networks: bool,
}

impl OriginPolicy {
    pub fn new(allow: Vec<String>, deny_private_networks: bool) -> Self {
        Self {
            allow,
            deny_private_networks,
        }
    }

    // Security boundary: untrusted browser navigation must never silently escape
    // this allowlist. Private-network denial is an independent veto applied first
    // and is never overridden by `allow` containing the same literal host string.
    pub fn is_allowed(&self, url: &url::Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };

        if self.deny_private_networks && is_private_or_loopback_host(host) {
            return false;
        }

        self.allow
            .iter()
            .any(|allowed| host == allowed || host.ends_with(&format!(".{allowed}")))
    }
}

// Recognizes localhost by name, then attempts to parse the host as an IP address
// to check loopback/private ranges via std's Ipv4Addr/Ipv6Addr methods.
fn is_private_or_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    // url::Url::host_str strips brackets from IPv6 literals, so parse directly.
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private(),
        Ok(IpAddr::V6(v6)) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_private())
        }
        Err(_) => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpenBrowserRequest {
    pub start_url: url::Url,
    pub headless: bool,
    pub profile: ProfilePolicy,
    pub bidi: BiDiRequirement,
    pub allowed_origins: OriginPolicy,
    pub viewport: Option<Viewport>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> url::Url {
        url::Url::parse(s).expect("valid test url")
    }

    #[test]
    fn test_is_allowed_exact_host_match() {
        let policy = OriginPolicy::new(vec!["erp.example.com".to_owned()], false);
        assert!(policy.is_allowed(&url("https://erp.example.com/path")));
    }

    #[test]
    fn test_is_allowed_subdomain_match() {
        let policy = OriginPolicy::new(vec!["example.com".to_owned()], false);
        assert!(policy.is_allowed(&url("https://erp.example.com/path")));
    }

    #[test]
    fn test_is_allowed_non_matching_host_denied() {
        let policy = OriginPolicy::new(vec!["erp.example.com".to_owned()], false);
        assert!(!policy.is_allowed(&url("https://evil.com/path")));
    }

    #[test]
    fn test_is_allowed_private_ip_denied_even_when_in_allow_list() {
        let policy = OriginPolicy::new(vec!["192.168.1.5".to_owned()], true);
        assert!(!policy.is_allowed(&url("http://192.168.1.5/path")));
    }

    #[test]
    fn test_is_allowed_localhost_denied_even_when_in_allow_list() {
        let policy = OriginPolicy::new(vec!["localhost".to_owned()], true);
        assert!(!policy.is_allowed(&url("http://localhost:8080/path")));
    }

    #[test]
    fn test_is_allowed_private_denial_independent_of_allowlist_presence() {
        // Private-network denial applies even when the allow list is otherwise empty
        // or unrelated: the deny check runs first and short-circuits.
        let policy = OriginPolicy::new(vec![], true);
        assert!(!policy.is_allowed(&url("http://127.0.0.1/path")));
        assert!(!policy.is_allowed(&url("http://10.0.0.5/path")));
        assert!(!policy.is_allowed(&url("http://172.16.0.1/path")));
    }

    #[test]
    fn test_is_allowed_public_host_not_in_allow_list_denied_regardless_of_deny_private_networks() {
        // Allow-listing is the only path to being allowed; disabling the private-network
        // veto never implicitly allows an unlisted public host.
        let policy = OriginPolicy::new(vec!["erp.example.com".to_owned()], false);
        assert!(!policy.is_allowed(&url("https://not-listed.example.org/path")));
    }

    #[test]
    fn test_is_allowed_no_host_denied() {
        let policy = OriginPolicy::new(vec!["example.com".to_owned()], false);
        // `data:` URLs have no host.
        assert!(!policy.is_allowed(&url("data:text/plain,hello")));
    }

    #[test]
    fn test_open_browser_request_serde_json_round_trip() {
        let request = OpenBrowserRequest {
            start_url: url("https://erp.example.com/login"),
            headless: true,
            profile: ProfilePolicy::Persistent {
                binding: "sales-team".to_owned(),
            },
            bidi: BiDiRequirement::Preferred,
            allowed_origins: OriginPolicy::new(vec!["erp.example.com".to_owned()], true),
            viewport: Some(Viewport {
                width: 1920,
                height: 1080,
            }),
        };

        let json = serde_json::to_string(&request).expect("request serializes");
        let decoded: OpenBrowserRequest =
            serde_json::from_str(&json).expect("request deserializes");
        assert_eq!(decoded, request);
    }

    #[test]
    fn test_profile_policy_ephemeral_serde_json_round_trip() {
        let policy = ProfilePolicy::Ephemeral;
        let json = serde_json::to_string(&policy).expect("policy serializes");
        let decoded: ProfilePolicy = serde_json::from_str(&json).expect("policy deserializes");
        assert_eq!(decoded, policy);
    }
}
