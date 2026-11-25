use std::collections::BTreeMap;
use std::net::SocketAddr;

use harw_onion::OnionLocator;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteMode {
    OnionOnly,
    InternetAndOnion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    SelectedApps,
    DeviceWide(RouteMode),
}

#[derive(Debug)]
pub enum Destination {
    Onion(OnionLocator),
    /// A destination validated as public by the packet or DNS path.
    PublicName {
        host: String,
        port: u16,
    },
    PublicIp(SocketAddr),
}

pub struct Policy {
    scope: Scope,
    rules: BTreeMap<u32, RouteMode>,
}

impl Policy {
    pub fn new(scope: Scope, rules: BTreeMap<u32, RouteMode>) -> Self {
        Self { scope, rules }
    }

    pub fn permits(&self, uid: Option<u32>, destination: &Destination) -> bool {
        let Some(uid) = uid else {
            return false;
        };
        let default = match self.scope {
            Scope::SelectedApps => None,
            Scope::DeviceWide(default) => Some(default),
        };
        let mode = self.rules.get(&uid).copied().or(default);
        matches!(
            (mode, destination),
            (Some(_), Destination::Onion(_))
                | (
                    Some(RouteMode::InternetAndOnion),
                    Destination::PublicName { .. } | Destination::PublicIp(_)
                )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_owner_is_denied_and_uid_rule_overrides_device_default() {
        let policy = Policy::new(
            Scope::DeviceWide(RouteMode::InternetAndOnion),
            BTreeMap::from([(1, RouteMode::OnionOnly)]),
        );
        assert!(!policy.permits(None, &Destination::PublicIp(([1, 1, 1, 1], 443).into())));
        assert!(!policy.permits(Some(1), &Destination::PublicIp(([1, 1, 1, 1], 443).into())));
        assert!(policy.permits(Some(2), &Destination::PublicIp(([1, 1, 1, 1], 443).into())));
    }

    #[test]
    fn selected_scope_only_admits_configured_uids() {
        let policy = Policy::new(
            Scope::SelectedApps,
            BTreeMap::from([(1, RouteMode::OnionOnly)]),
        );
        let destination = Destination::Onion(
            "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443"
                .parse()
                .unwrap(),
        );
        assert!(policy.permits(Some(1), &destination));
        assert!(!policy.permits(Some(2), &destination));
    }
}
